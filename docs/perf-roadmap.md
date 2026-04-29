# Performance roadmap

Rolling list of throughput and latency optimisations under investigation. Each
entry links to the audit doc and the tracking issue. Status is one of
`audit`, `phase-N`, or `landed`.

| Topic | Status | Audit | Tracking |
|-------|--------|-------|----------|
| splice/vmsplice for SSH stdio | audit | [docs/audits/splice-ssh-stdio.md](audits/splice-ssh-stdio.md) | #1860 |

Notes:

- Add new entries above the table footer. Keep them sorted by topic.
- An entry leaves this list once every phase has landed and the corresponding
  benchmark dashboard shows the expected savings.
- The audit doc is the source of truth for phased plan, risks, and follow-up
  tasks. This file is the index.
