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
- `premium-notice.txt` is a UHC letter that is not an EOB.

Dummy shapes, enforced by `tests/no_personal_data.rs`:

| Field | Shape |
|---|---|
| Member ID | `SYNM` + 6 digits |
| Group number | `SYNG` + 4 digits |
| Claim number | `SYNC` + 8 digits |
| Patient account number | `SYNA` + 4 digits |
| People | `ALEX`, `SAM`, `JORDAN`, surname `EXAMPLE` |
| Providers, employer | `EXAMPLE …`, `SAMPLE …`, `TEST …` |
| Phone numbers | `555-01xx` |

A file whose name ends in `-statement.txt` is a statement (`make smoke`
counts them); any other file here is a PDF that is not one.

## site/

The member site's answers to uhc's queries (`src/site/queries/`), one per
answer shape. Their structure is the real answers': the same keys and
nesting, the same types (amounts are strings, `"123.45"`), `null` where the
site sends `null` and the key left out where the site leaves it out (the
profile drops null fields, so `groupName` is absent). Every value is
invented, in the dummy shapes above and below.

| File | The answer to |
|---|---|
| `claims.json` | `claims`: the profile and `healthcareClaims.claims[]` |
| `claim-detail-*.json` | `claimDetails`, one claim each: office visit, professional (CPT and HCPCS), institutional (a revenue code), dental (CDT, a tooth range), reprocessed, denied, suffixed (procedure codes with one more character), written-off (a line written off in full, left out of the billed total but counted in the discount) |
| `spending.json` | `spending`: the profile, `benefits[]` (maximums), `accumulators[]` |
| `documents.json` | `documents`: `multiDocTypeListSummary.documents[]`, with one of the plan's general forms (no member, no date), which uhc leaves out |
| `unauthorized.json` | any read without a session: `401 application/json` |
| `edge-403.html` | a request without a browser's user agent: CloudFront's `403 text/html` page |
| `graphql-errors.json` | a query the API refuses: GraphQL `errors` and `data: null`, in the GraphQL specification's shape |

The revenue line in `claim-detail-institutional.json` is shaped by the
query (`revenues { code description }`); no captured answer has carried a
revenue code yet. In `claim-detail-suffixed.json` the `serviceInfo` and
`providedServiceInfo` codes repeat the procedure code, character included,
as the site's do on such a line. `graphql-errors.json` follows the GraphQL specification,
not a captured answer.

| Field | Shape |
|---|---|
| `firstName`, `lastName`, `memberName` | `Alex`, `Sam`, `Jordan`, `Example`; `ALEX EXAMPLE` |
| `memberId.id`, `groupNumber` | `SYNM` + 6 digits, `SYNG` + 4 digits |
| A medical `claimId` | `SYNC` + digits, with `-NN` when the suffix is in the id |
| A pharmacy or dental `claimId`, `prescriptionNumber` | zeros then one digit, or `555…` |
| `claimKey` | `SYNK-0…`, 8 characters: too short for gitleaks' generic-api-key rule (10 or more), and uhc never decodes a key |
| `documentId` | `00000000-0000-4000-8000-0000000000NN` |
| `providerName` | `EXAMPLE …` |
