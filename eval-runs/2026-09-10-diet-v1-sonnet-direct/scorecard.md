# Scorecard — diet-v1

Driver: `direct` · wire: messages · model: claude-sonnet-5 · index: grep

Target: endpoint `https://api.anthropic.com`, model `claude-sonnet-5`

| Class | Pass rate | Mean latency | Mean tool calls |
|---|---|---|---|
| diet-correctness | 1/1 (100%) | 15567 ms | 2.0 |
| diet-extraction | 3/3 (100%) | 17281 ms | 3.3 |
| diet-safety | 0/1 (0%) | 15048 ms | 2.0 |
| **TOTAL** | **4/5 (80%)** | **16491 ms** | **2.8** |
