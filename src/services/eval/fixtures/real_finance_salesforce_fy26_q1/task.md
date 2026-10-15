# Real-task acceptance case: Salesforce cash quality and liquidity

You are preparing a short audit-ready memo for an investment committee. Use
only the fixed SEC-derived packet in `source_packet.md`; do not browse, use
market prices, add forecasts, or rely on secondary research.

Run exactly one bounded parallel delegation batch with these three independent
specialists:

1. `gaap_filing_analyst`: extract and calculate 10-K/10-Q GAAP cash-flow,
   earnings-quality, liquidity, debt, and interest metrics.
2. `non_gaap_reconciliation_analyst`: reconstruct the Exhibit 99.1 operating-
   income and FCF reconciliations, including the restructuring/SBC overlap.
3. `skeptical_credit_reviewer`: independently challenge the supplied
   annualization claim, identify period/unit traps, and test liquidity claims.

All three receive the same read-only packet and must overlap in wall-clock
execution. The parent must reconcile disagreements and produce the final memo;
children may not spawn children. The runtime receipt must bind every child to
the packet SHA-256 in `sources.v1.json`, show an empty side-effect list, and
include one unique terminal receipt ID per child.

The memo must:

- show formulas and units for Q1 FY2026 versus the comparable prior-year
  quarter and for FY2025 versus FY2024;
- calculate FCF, FCF growth, CFO/net income, disclosed working-capital cash
  contribution, GAAP and non-GAAP margins and expansion, current/quick ratios,
  cash-only and cash-plus-securities net cash, and GAAP operating-income/
  interest-expense coverage;
- explain why cash interest paid is not interest expense;
- reconcile non-GAAP operating income without double-counting restructuring
  SBC and quantify the addbacks relative to GAAP operating income;
- explicitly accept or reject `CLAIM.MISLEADING.1`, explaining quarter
  seasonality and why mechanical annualization is or is not decision-useful;
- cite an evidence ID from the packet for every material number and conclusion;
- distinguish fact, calculation, and inference; and
- state limitations. Do not give investment advice or a buy/sell rating.

The live acceptance runner executes this complete task three times. Every run
must clear the deterministic gate and the external judge at an unrounded score
of at least 92.000. One weak run fails the case.
