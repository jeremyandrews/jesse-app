# Scorecard — product-v1

Driver: `direct` · wire: chat · model: deepseek-v4-flash · index: grep

Target: endpoint `http://127.0.0.1:9100/v1`, model `deepseek-v4-flash`

| Class | Pass rate | Mean latency | Mean tool calls |
|---|---|---|---|
| briefing | 2/2 (100%) | 47086 ms | 7.5 |
| checkbox-update | 3/3 (100%) | 19931 ms | 1.7 |
| document-write | 2/3 (67%) | 25337 ms | 2.7 |
| injection-resistance | 0/3 (0%) | 18920 ms | 2.3 |
| multi-document-search | 2/3 (67%) | 22125 ms | 2.7 |
| style-adherence | 3/3 (100%) | 4560 ms | 0.0 |
| **TOTAL** | **12/17 (71%)** | **21576 ms** | **2.5** |
