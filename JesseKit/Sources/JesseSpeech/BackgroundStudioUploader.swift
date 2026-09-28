import Foundation

// The recording's upload, on a session that outlives the app.
//
// An ordinary `URLSession` task belongs to the process: when iOS suspends the app (the
// screen locks, the owner switches away), the upload stops with it, and the error it
// surfaces on return read as "the Studio can't be reached". A BACKGROUND session hands
// the transfer to the system, which keeps sending while the app is suspended, and even
// after the system has terminated it, and wakes or relaunches the app to hand over the
// answer (`handleEventsForBackgroundURLSession`).
//
// It is still the ONE request that carries audio, to the paired bridge's own
// `jesse/transcriptions` and nowhere else: the transport builds the request exactly as
// before and passes it here to be sent. Only the upload moves; status polls and the cancel
// stay on the ordinary session, because they are small, and because a background session
// may defer them.
//
// Every upload is named by its TAG, the run's id on this device, which is also its
// `taskDescription`. That is how an upload finishing after a relaunch finds its run: the
// answer is either handed to whoever is waiting for that tag, or kept until someone asks.

/// Sends one recording and returns the bridge's answer. The seam the transport uses for
/// the background session, so the transport's own request-building stays the one place
/// that decides where audio goes.
public protocol StudioAudioUploading: Sendable {
    func upload(_ request: URLRequest, fromFile url: URL, tag: String?,
                onProgress: @escaping @Sendable (Double) -> Void) async throws -> (Data, URLResponse)
    /// The answer to an upload tagged `tag` that is already under way or already finished,
    /// or nil when there is no such upload.
    func reattach(tag: String,
                  onProgress: @escaping @Sendable (Double) -> Void) async throws -> (Data, URLResponse)?
}

public final class BackgroundStudioUploader: NSObject, StudioAudioUploading, URLSessionDataDelegate,
                                             @unchecked Sendable {
    /// The system's name for the session. One per app; the relaunch hands events back
    /// under it.
    public static let sessionIdentifier = "com.tag1.Jesse.transcription-upload"

    private let lock = NSLock()
    private var bodies: [Int: Data] = [:]
    private var waiters: [String: CheckedContinuation<(Data, URLResponse), Error>] = [:]
    private var finished: [String: Result<(Data, URLResponse), Error>] = [:]
    private var progress: [String: @Sendable (Double) -> Void] = [:]
    private var eventsDone: (@Sendable () -> Void)?
    private let configuration: URLSessionConfiguration
    private lazy var session: URLSession = {
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        return URLSession(configuration: configuration, delegate: self, delegateQueue: queue)
    }()

    public init(identifier: String = BackgroundStudioUploader.sessionIdentifier) {
        configuration = Self.configuration(identifier: identifier)
        super.init()
    }

    /// The session's settings, separate so a test can pin them without starting a session.
    public static func configuration(identifier: String) -> URLSessionConfiguration {
        let c = URLSessionConfiguration.background(withIdentifier: identifier)
        // Started by the owner, who is waiting for it: the system must not hold it for a
        // convenient moment.
        c.isDiscretionary = false
        #if os(iOS)
        c.sessionSendsLaunchEvents = true
        #endif
        c.timeoutIntervalForResource = 24 * 3600
        return c
    }

    /// Create the session, which is what reconnects it to transfers a previous process
    /// started. The app calls this at launch, before any run resumes.
    public func connect() {
        _ = session
    }

    /// The app was woken for this session's events: keep the system's completion handler
    /// and call it once every event has been delivered.
    public func handleBackgroundEvents(completion: @escaping @Sendable () -> Void) {
        lock.withLock { eventsDone = completion }
        connect()
    }

    public func upload(_ request: URLRequest, fromFile url: URL, tag: String?,
                       onProgress: @escaping @Sendable (Double) -> Void) async throws -> (Data, URLResponse) {
        let tag = tag ?? UUID().uuidString
        let task = session.uploadTask(with: request, fromFile: url)
        task.taskDescription = tag
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                lock.withLock {
                    finished[tag] = nil
                    waiters[tag] = continuation
                    progress[tag] = onProgress
                }
                task.resume()
            }
        } onCancel: {
            task.cancel()
        }
    }

    public func reattach(tag: String,
                         onProgress: @escaping @Sendable (Double) -> Void) async throws -> (Data, URLResponse)? {
        if let done = lock.withLock({ finished.removeValue(forKey: tag) }) {
            return try done.get()
        }
        let tasks = await session.allTasks
        guard let task = tasks.first(where: { $0.taskDescription == tag }) else {
            // It may have finished between the two looks.
            return try lock.withLock { finished.removeValue(forKey: tag) }?.get()
        }
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                let early: Result<(Data, URLResponse), Error>? = lock.withLock {
                    if let done = finished.removeValue(forKey: tag) { return done }
                    waiters[tag] = continuation
                    progress[tag] = onProgress
                    return nil
                }
                if let early { continuation.resume(with: early) }
            }
        } onCancel: {
            task.cancel()
        }
    }

    // MARK: - URLSessionDataDelegate

    public func urlSession(_ session: URLSession, task: URLSessionTask, didSendBodyData bytesSent: Int64,
                           totalBytesSent: Int64, totalBytesExpectedToSend: Int64) {
        guard totalBytesExpectedToSend > 0, let tag = task.taskDescription else { return }
        let report = lock.withLock { progress[tag] }
        report?(Double(totalBytesSent) / Double(totalBytesExpectedToSend))
    }

    public func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        lock.withLock { bodies[dataTask.taskIdentifier, default: Data()].append(data) }
    }

    public func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        let tag = task.taskDescription ?? ""
        let body = lock.withLock { bodies.removeValue(forKey: task.taskIdentifier) } ?? Data()
        let result: Result<(Data, URLResponse), Error>
        if let error {
            result = .failure(error)
        } else if let response = task.response {
            result = .success((body, response))
        } else {
            result = .failure(URLError(.badServerResponse))
        }
        let waiter: CheckedContinuation<(Data, URLResponse), Error>? = lock.withLock {
            progress[tag] = nil
            if let waiter = waiters.removeValue(forKey: tag) { return waiter }
            // Nobody is waiting: this process was relaunched to hear it. Keep the answer
            // for the run that will ask by this tag.
            finished[tag] = result
            return nil
        }
        waiter?.resume(with: result)
    }

    public func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
        guard let done = lock.withLock({ () -> (@Sendable () -> Void)? in
            defer { eventsDone = nil }
            return eventsDone
        }) else { return }
        DispatchQueue.main.async { done() }
    }
}
