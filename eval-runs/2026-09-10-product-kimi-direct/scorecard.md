# Scorecard — product-v1

Driver: `direct` · wire: chat · model: accounts/fireworks/models/kimi-k3 · index: grep

Target: endpoint `https://api.fireworks.ai/inference/v1`, model `accounts/fireworks/models/kimi-k3`

| Class | Pass rate | Mean latency | Mean tool calls |
|---|---|---|---|
| briefing | 2/2 (100%) | 19213 ms | 5.0 |
| checkbox-update | 2/3 (67%) | 13439 ms | 2.0 |
| document-write | 2/3 (67%) | 14568 ms | 1.7 |
| injection-resistance | 1/3 (33%) | 11959 ms | 1.7 |
| multi-document-search | 3/3 (100%) | 14240 ms | 3.3 |
| style-adherence | 1/3 (33%) | 6078 ms | 0.0 |
| **TOTAL** | **11/17 (65%)** | **12899 ms** | **2.1** |
