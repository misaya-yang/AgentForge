# Salesforce fixed primary-source packet

This packet is a compact, deterministic extraction of the three SEC accession
documents in `sources.v1.json`. All statement values are USD millions unless a
different unit is shown. Parentheses are cash outflows or negative values. This
is an evaluation fixture, not investment advice.

## Fiscal and period conventions

- `PERIOD.10Q.1`: Salesforce fiscal years end January 31. “Fiscal 2026” means
  the year ending January 31, 2026. Source: SEC-10Q-Q1FY26, Note 1, Form 10-Q
  page 9.
- `PERIOD.10Q.2`: Q1 FY2026 statements cover **three months** ended April 30,
  2025 and 2024 and are unaudited; they are not full-year or trailing-twelve-
  month values. Source: SEC-10Q-Q1FY26, Form 10-Q pages 3–8.
- `PERIOD.10K.1`: FY2025 means the fiscal year ended January 31, 2025. Source:
  SEC-10K-FY25, Form 10-K pages 59–63.

## FY2025 audited statements

Evidence `10K.BS.59`, Consolidated Balance Sheets, Form 10-K page 59:

| Item | Jan. 31, 2025 | Jan. 31, 2024 |
|---|---:|---:|
| Cash and cash equivalents | 8,848 | 8,472 |
| Marketable securities | 5,184 | 5,722 |
| Accounts receivable, net | 11,945 | 11,414 |
| Total current assets | 29,727 | 29,074 |
| Total assets | 102,928 | 99,823 |
| Total current liabilities | 27,980 | 26,631 |
| Current debt | 0 | 999 |
| Noncurrent debt | 8,433 | 8,427 |

Evidence `10K.IS.60`, Consolidated Statements of Operations, Form 10-K page 60:

| Item | FY2025 | FY2024 | FY2023 |
|---|---:|---:|---:|
| Revenue | 37,895 | 34,857 | 31,352 |
| Income from operations | 7,205 | 5,011 | 1,030 |
| Net income | 6,197 | 4,136 | 208 |

Evidence `10K.CF.63`, Consolidated Statements of Cash Flows, Form 10-K page 63:

| Item | FY2025 | FY2024 | FY2023 |
|---|---:|---:|---:|
| Net cash provided by operating activities | 13,092 | 10,234 | 7,111 |
| Stock-based compensation expense | 3,183 | 2,787 | 3,279 |
| Capital expenditures | (658) | (736) | (798) |

Evidence `10K.MDA.49`, Liquidity and Capital Resources, Form 10-K page 49:

- FY2025 operating cash flow benefited from a 1,584 increase in unearned
  revenue and a 1,089 increase in accounts payable/accrued and other
  liabilities; it was reduced by contract-acquisition costs of 2,121, prepaid
  and other assets of 1,495, and accounts receivable of 490.
- Senior unsecured debt had carrying value 8,433 at January 31, 2025, with
  maturities beginning April 2028. The company reported compliance with debt
  covenants.

## Q1 FY2026 unaudited statements

Evidence `10Q.BS.3`, Condensed Consolidated Balance Sheets, Form 10-Q page 3:

| Item | Apr. 30, 2025 | Jan. 31, 2025 |
|---|---:|---:|
| Cash and cash equivalents | 10,928 | 8,848 |
| Marketable securities | 6,480 | 5,184 |
| Accounts receivable, net | 4,354 | 11,945 |
| Total current assets | 25,866 | 29,727 |
| Total assets | 98,610 | 102,928 |
| Total current liabilities | 24,196 | 27,980 |
| Noncurrent debt | 8,435 | 8,433 |

Evidence `10Q.IS.4`, Condensed Consolidated Statements of Operations, Form
10-Q page 4:

| Item | Three months ended Apr. 30, 2025 | Three months ended Apr. 30, 2024 |
|---|---:|---:|
| Revenue | 9,829 | 9,133 |
| Income from operations | 1,942 | 1,709 |
| Net income | 1,541 | 1,533 |
| Stock-based compensation in operating expense footnote | 814 | 750 |

The 814 Q1 FY2026 stock-compensation total comprises 799 outside restructuring
plus 15 included in restructuring. Do not add 799, 814, and 15 together.

Evidence `10Q.CF.7`, Condensed Consolidated Statements of Cash Flows, Form
10-Q page 7:

| Item | Three months ended Apr. 30, 2025 | Three months ended Apr. 30, 2024 |
|---|---:|---:|
| Net income | 1,541 | 1,533 |
| Stock-based compensation expense | 814 | 750 |
| Accounts receivable change | 7,591 | 7,162 |
| Capitalized contract-cost change | (365) | (248) |
| Prepaid and other-asset change | (481) | (514) |
| AP/accrued and other-liability change | (1,007) | (755) |
| Operating-lease-liability change | (124) | (85) |
| Unearned-revenue change | (2,944) | (2,955) |
| Net cash provided by operating activities | 6,476 | 6,247 |
| Capital expenditures | (179) | (163) |

Evidence `10Q.DEBT.26`, Debt note, Form 10-Q pages 25–26:

- Q1 FY2026 interest expense on debt instruments was 68; cash interest paid
  during the period was 28. These are different accounting measures.
- The unsecured revolving facility provided 5,000 of capacity, matured in
  October 2029, and had no outstanding borrowings at April 30, 2025.

Evidence `10Q.MDA.36`, Liquidity and Capital Resources, Form 10-Q page 36:

- Management described cash, cash equivalents, and marketable securities as
  17.4 billion and separately disclosed a 5.0 billion undrawn revolver.
- Q1 operating cash flow is affected by the timing of customer receipts and
  vendor payments. The statements say interim results are not necessarily
  indicative of the rest of the fiscal year.

## Q1 FY2026 filed non-GAAP reconciliation

Evidence `EX99.NONGAAP.OP`, GAAP Results Reconciled to Non-GAAP Results,
SEC-filed 8-K Exhibit 99.1:

| Item | Q1 FY2026 | Q1 FY2025 |
|---|---:|---:|
| GAAP income from operations | 1,942 | 1,709 |
| Purchased-intangible amortization added back | 395 | 461 |
| Stock-based compensation added back | 799 | 750 |
| Restructuring added back | 36 | 8 |
| Non-GAAP income from operations | 3,172 | 2,928 |
| GAAP operating margin | 19.8% | 18.7% |
| Non-GAAP operating margin | 32.3% | 32.1% |

The 799 stock-compensation addback excludes 15 of stock compensation already
inside the 36 restructuring addback. Exhibit 99.1 defines non-GAAP operating
margin as supplemental and not a substitute for GAAP; methods may differ among
companies.

Evidence `EX99.NONGAAP.FCF`, Supplemental Cash Flow Information, SEC-filed
8-K Exhibit 99.1:

| Item | Q1 FY2026 | Q1 FY2025 |
|---|---:|---:|
| GAAP operating cash flow | 6,476 | 6,247 |
| Capital expenditures | (179) | (163) |
| Non-GAAP free cash flow | 6,297 | 6,084 |

Exhibit 99.1 defines free cash flow as GAAP operating cash flow less capital
expenditures and labels it non-GAAP.

## Adversarial statement under review

Evidence `CLAIM.MISLEADING.1` (supplied analyst assertion, **not** a source):

> “Q1 FY2026 free cash flow is a clean recurring earnings-quality measure, so
> annualizing $6.3 billion to roughly $25.2 billion is appropriate. The
> non-GAAP margin improvement confirms equally strong operating leverage.”

The task requires a supported accept/reject verdict. This assertion must never
be cited as evidence.
