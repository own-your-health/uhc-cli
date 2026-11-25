# Test fixtures

Every file here is **synthetic**. No value in them is a real member's, and no
real statement was used to write them.

- `eob/*.txt` are what `pdftotext -layout` makes of a UnitedHealthcare
  Explanation of Benefits statement, written by hand after the layout of UHC's
  public explainer (`understanding-your-eob-flier.pdf`, see `docs/api.md`). Each
  starts with `SYNTHETIC TEST DATA`, the marker `uhc` treats as synthetic.
- `damaged-statement.txt` is deliberately incomplete: no member id, no claim
  number, a balances row short of values, a service line with eight amounts
  and a total that does not add up. It exercises the reader's issue codes.
- `2026-05-07-statement.txt` holds three claims in one "Claim detail for"
  block, on one page; `2026-04-15-statement.txt` a pharmacy benefit's claim;
  `2026-10-09-statement.txt` a claim that continues on the next page, then a
  page that holds only processing codes under a "continued" heading. Four of
  the claims, and the three members, are also in `site/`.
- `premium-notice.txt` is a UHC letter that is not an EOB.

Dummy shapes, enforced by `tests/no_personal_data.rs`:

| Field | Shape |
|---|---|
| Member ID | `SYNM` + 6 digits |
| Group number | `SYNG` + 4 digits |
| Claim number | `SYNC` + 8 digits |
| Patient account number | `SYNA` + 4 digits; a pharmacy claim's, `555…`, 17 digits |
| People | `ALEX`, `SAM`, `JORDAN`, surname `EXAMPLE` |
| Providers, employer | `EXAMPLE …`, `SAMPLE …`, `TEST …` |
| Phone numbers | `555-01xx` |

A file whose name ends in `-statement.txt` is a statement (`make smoke`
counts them); any other file here is a PDF that is not one.

## site/

Answers of the member site (myuhc.com), as `uhc` receives them. The field
names, types, codes and statuses follow the site's answers, with null where the
site sends null and a key left out where the site leaves it out. Everything
else is invented: every claim, date, amount and provider, and which of them go
together. The household is Alex (subscriber), Sam (spouse) and Jordan (child)
Example. Amounts satisfy the equations in `docs/api.md`, except in the two
`left-out` claims, whose totals leave lines out.

| File | What it holds |
|---|---|
| `claims.json` | The profile and 14 rows for 12 claims: one reprocessed (two versions), one pharmacy claim reversed, one in process, one denied to the provider, one dental, the rest medical and pharmacy, in and out of network |
| `claim-detail-reprocessed.json` | Professional, the latest version's detail |
| `claim-detail-institutional.json` | Hospital: revenue codes, a line of two days, a CPT code with one more character, HCPCS codes |
| `claim-detail-professional.json` | Professional: a HCPCS code with one more digit, a line with no remark code |
| `claim-detail-left-out-discount.json` | Totals that leave two lines out, with their write-offs still in the discount |
| `claim-detail-left-out-owed.json` | Totals that leave out a line with an amount owed |
| `claim-detail-provider-denied.json` | `PROVIDER_DENIED`: no network, everything written off |
| `claim-detail-no-suffix.json` | A claim id with no suffix; copays, and a line not covered |
| `claim-detail-out-of-network.json` | Out of network: coinsurance and amounts not covered |
| `claim-detail-dental.json` | CDT codes, `toothNumber`, `allowedAmount` |
| `documents.json` | EOBs (one of two claims, one of a pharmacy claim, one dental), a health statement, a letter, and a general form addressed to no one |
| `spending.json` | Maximums and progress of a calendar-year medical plan, each member's and the family's; dental and pharmacy besides |
| `unauthorized.json` | The answer to a session that has ended |
| `graphql-errors.json` | A refused query |
| `edge-403.html` | The CDN's refusal page |

| Field | Shape |
|---|---|
| `firstName`, `lastName`, `memberName` | `Alex`, `Sam`, `Jordan`, `Example`; `ALEX EXAMPLE` |
| `memberId.id`, `groupNumber` | `SYNM` + 6 digits, `SYNG` + 4 digits |
| A medical `claimId` | `SYNC` + digits, with `-NN` when the suffix is in the id |
| A pharmacy or dental `claimId`, `prescriptionNumber` | zeros then one digit, or `555…` |
| `claimKey` | `SYNK-0…`, 8 characters: too short for gitleaks' generic-api-key rule (10 or more), and uhc never decodes a key |
| `documentId` | `00000000-0000-4000-8000-0000000000NN` |
| `providerName` | `EXAMPLE …` |
