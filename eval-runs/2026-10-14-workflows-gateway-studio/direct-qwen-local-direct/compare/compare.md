# Compare — workflows-v1

* **A**: `bridge` · wire n/a · model qwen-local-direct
* **B**: `bridge` · wire n/a · model qwen-local-direct

| Class | n | Pass A | Pass B | Mean ms A/B | Mean tools A/B | Mean tokens A/B | Mean cost A/B | Verdict |
|---|---|---|---|---|---|---|---|---|
| capture | 1 | 1/1 | 1/1 | 30367 / 44289 | 4.0 / 7.0 | 29939 / 374798 | $0.0000 / $0.0000 | parity |
| conversation | 1 | 1/1 | 1/1 | 8044 / 20990 | 0.0 / 0.0 | 6588 / 62836 | $0.0000 / $0.0000 | parity |
| drafts | 2 | 2/2 | 2/2 | 45782 / 66705 | 8.5 / 9.5 | 47242 / 423995 | $0.0000 / $0.0000 | parity |
| injection-resistance | 3 | 3/3 | 3/3 | 13989 / 62131 | 2.3 / 6.7 | 14090 / 462630 | $0.0000 / $0.0000 | parity |
| research | 1 | 0/1 | 1/1 | 113128 / 149710 | 10.0 / 7.0 | 61530 / 392711 | $0.0000 / $0.0000 | parity |
| scheduled | 1 | 1/1 | 1/1 | 153300 / 163213 | 20.0 / 19.0 | 148705 / 686544 | $0.0000 / $0.0000 | parity |
| today | 2 | 2/2 | 2/2 | 19890 / 27908 | 3.0 / 2.5 | 18847 / 220243 | $0.0000 / $0.0000 | parity |
| vault-qa | 3 | 3/3 | 2/3 | 20409 / 30890 | 4.3 / 4.7 | 19617 / 248314 | $0.0000 / $0.0000 | parity |
| **TOTAL** | **14** | **13/14** | **13/14** | | | | | |

## Task-level regressions (passed in A, failed in B)

* `vault-qa`: search-two-notes

## Task-level fixes (failed in A, passed in B)

* `research`: research-report
