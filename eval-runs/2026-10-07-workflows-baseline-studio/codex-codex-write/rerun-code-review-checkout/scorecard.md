# Scorecard — workflows-v1 (code-review-checkout rerun)

Driver: `bridge` · wire: n/a · model: codex-write · index: n/a

Target: endpoint `spawned jesse-bridge 0.168.0`, model `codex-write`

Harness: codex · runs per task: 3 (pass^3)

| Class | Pass rate | Mean latency | Mean tool calls |
|---|---|---|---|
| code | 1/1 (100%) | 46610 ms | 6.0 |
| **TOTAL** | **1/1 (100%)** | **46610 ms** | **6.0** |

## pass^3 per task

A task passes only when all 3 runs passed.

| Task | Class | pass^k | Runs passed | Failed assertion kinds |
|---|---|---|---|---|
| code-review-checkout | code | PASS | 3/3 |  |

## Latency

| Measure | p50 | p95 | n |
|---|---|---|---|
| submit to first streamed event | 7051 ms | 7157 ms | 3 |
| submit to first model token | 7051 ms | 7157 ms | 3 |
| submit to result | 50323 ms | 95086 ms | 3 |
