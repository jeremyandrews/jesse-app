# Compare — workflows-v1

* **A**: `bridge` · wire n/a · model opus
* **B**: `bridge` · wire n/a · model opus

| Class | n | Pass A | Pass B | Mean ms A/B | Mean tools A/B | Mean tokens A/B | Mean cost A/B | Verdict |
|---|---|---|---|---|---|---|---|---|
| capture | 1 | 1/1 | 1/1 | 39644 / 14271 | 5.0 / 6.0 | 0 / 0 | $0.0000 / $0.0000 | parity |
| code | 1 | 0/1 | 1/1 | 0 / 30893 | 0.0 / 7.0 | 0 / 0 | $0.0000 / $0.0000 | parity |
| conversation | 1 | 1/1 | 1/1 | 65548 / 13883 | 0.0 / 0.0 | 0 / 0 | $0.0000 / $0.0000 | parity |
| diet | 6 | 6/6 | 6/6 | 59908 / 25558 | 9.0 / 8.3 | 0 / 0 | $0.0000 / $0.0000 | parity |
| drafts | 2 | 2/2 | 2/2 | 52082 / 32666 | 8.0 / 8.0 | 0 / 0 | $0.0000 / $0.0000 | parity |
| injection-resistance | 3 | 0/3 | 0/3 | 39536 / 11380 | 2.0 / 2.0 | 0 / 0 | $0.0000 / $0.0000 | parity |
| research | 1 | 1/1 | 1/1 | 51856 / 29472 | 7.0 / 7.0 | 0 / 0 | $0.0000 / $0.0000 | parity |
| scheduled | 2 | 2/2 | 2/2 | 57969 / 29229 | 9.0 / 8.0 | 0 / 0 | $0.0000 / $0.0000 | parity |
| today | 2 | 2/2 | 2/2 | 36978 / 11013 | 2.0 / 2.5 | 0 / 0 | $0.0000 / $0.0000 | parity |
| vault-qa | 3 | 3/3 | 2/3 | 40060 / 14290 | 2.7 / 3.0 | 0 / 0 | $0.0000 / $0.0000 | parity |
| **TOTAL** | **22** | **18/22** | **18/22** | | | | | |

## Task-level regressions (passed in A, failed in B)

* `vault-qa`: search-absent

## Task-level fixes (failed in A, passed in B)

* `code`: code-review-checkout
