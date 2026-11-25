# From a statement or the member site to FHIR

`uhc records export` maps what the EOB statements print (`--files`), or
what the member site answers (`--profile`), into FHIR R4 (4.0.1). It
follows CARIN Blue Button 2.0's (STU2) element choices and code systems
where the source carries the value, and does not claim the CARIN profiles
(`meta.profile`): neither source gives the diagnoses the profiles require.
No FHIR library is involved; `src/fhir.rs` writes the JSON.

| Source | `meta.source` | `meta.tag` (system `https://github.com/piekstra/uhc-cli/fhir/tag`) |
| --- | --- | --- |
| Statements | `uhc:eob-pdf` | `mapped-from-eob-statement` |
| Member site | `uhc:member-site` | `mapped-from-member-site` |

The tag system and the benefit-type system below are identifiers, not
links. They keep the `piekstra` owner the repository had when they were
minted, so records already imported keep matching what uhc writes now.

`meta.lastUpdated` says when a resource's content was current, by
medical-common DESIGN §1.4. A store keeps the version with the later time:

| Source | Resources | `meta.lastUpdated` |
| --- | --- | --- |
| Statements | `ExplanationOfBenefit`, `DocumentReference`, `Binary` | the statement's date: the date printed in its footer, else the "as of" date printed over the plan balances |
| Statements | `Patient`, `Coverage`, `Organization` | the member's latest statement's date, taken the same way. They are built from every statement in the folder, whatever `--since` covers: each field is the newest statement's that prints it (the full name and relationship from those addressed to the member) |
| Member site | `Patient`, `Coverage`, `Organization`, `ExplanationOfBenefit` | the time the site was read: the stream header's `pulled_at` |
| Member site | `DocumentReference`, `Binary` | the document's creation date, as the documents read lists it |

A date is written as that day at `00:00:00Z`, as printed, with no
time-zone conversion. A statement with no date (`no_statement_date` in
`import check`), or a site document with none, leaves it out. A document
keeps the same stamp on every read, so the same document is the same
resource. A claim read from the site again carries the new read's time.
The site read after a statement therefore wins over it in either import
order, and a statement dated after the last site read (a claim reprocessed
since) wins over the site's version.

## Identity

| Thing | Id | Why |
| --- | --- | --- |
| Member | `m` + 12 hex of SHA-256(member id \| first name) | one per person on the plan; the same across statements; no name in it. The member id is the site's form: older statements print one letter before its digits (`A` + nine digits where the site and current statements have the nine digits), and that letter is dropped (`eob::canonical_member_id`) |
| Family | `family-` + 12 hex of SHA-256(member id \| "") | the family rows of the plan balances |
| Coverage | `cov-` + 12 hex of SHA-256(member id \| member \| group) | one per member and group |
| Claim | `c` + the claim number | stable across reprocessing |
| Claim, on the site | `c` + the `claimId` without its `-NN` part, then `claimIdSuffix` | the site lists `SYNC000000-06` / `06` for the statement's `SYNC00000006`, and `SYNC000000` / `08` for `SYNC00000008` |
| Pharmacy claim | `rx` + the first 17 digits of the pharmacy claim number | a medical statement prints a pharmacy benefit's claim from "Provider: Pharmacy" under a claim number of its own, with the pharmacy claim number, cut to 17 digits, as its "Patient account number"; the site lists it under the whole (18-digit) number |
| Claim without a number | `x` + the first 10 hex of the document id + its index | a damaged statement |
| Statement | first 16 hex of the PDF's SHA-256 | the same file saved twice is one statement |
| DocumentReference, Binary | `doc-` / `bin-` + the statement id + `-` + the member id | a statement carrying two members' claims is a document of each, with its own subject |
| Site document | the site's `documentId` (a UUID) | `doc-` / `bin-` + it + `-` + the member id. The site's PDF bytes differ on each download, so a statement read from the files and the same statement on the site are two documents |

Both sources give a member, a claim and its coverage the same id, so
medrec holds one record whichever source last wrote it; the site's
version carries more (the codes below). The site's member is its
`planCoverages[]` medical `memberId.id` and `userInfo.firstName`. A
statement's pharmacy-claim rule and the dental claim id rest on few
samples (`docs/api.md`).

A claim printed on more than one statement (reprocessed) is the latest
statement's version; `versions` in `claims list` counts them. Two
statements dated the same day are ordered by statement id, so which is
"latest" between them is arbitrary but stable.

Statements print first names only. Two members of one plan with the same
first name are one member to `uhc`.

### The member id and medical-common's rule

The member id is medical-common DESIGN §1.3's patient id for a mapped
source:
- It comes from the plan's member id and the person's first name. Both are
  on every statement and in every site sign-in that shows the person, so a
  dependent seen from a parent's sign-in and from their own has one id. The
  first name tells apart the people who share the plan's member id.
- It is `m` and the first 12 hex digits of a SHA-256 digest of the two,
  joined with `|`, which neither contains: a member id is digits (or
  `unknown-` and hex for a statement that prints none), and a first name is
  letters.
- Both sources derive it the same way.

A store keys patients on it, so a new derivation would file every member
again.

The digest keeps the member id out of plain sight, nothing more. Trying
every nine-digit member id with a first name recovers it, so it is handled
as the member id: never in a fixture, doc, commit, issue, log line or error
message. uhc's error text names no member, and a statement read with issues
is named by its own id, not its `DocumentReference`'s, which carries the
member's. The family and coverage ids above are built the same way and
handled the same way.

A statement without a member ID gets a stand-in (`unknown-…`) to group its
claims. It is never exported as UHC's identifier: that `Patient` has no
`identifier` and that `Coverage` no `subscriberId`.

The site lists a reprocessed claim once per version (the same id, a later
`processedDate`) and a reversed pharmacy claim again with negative
amounts. Each is one record: the latest version's, and a reversal
`status` `cancelled` with the original's amounts.

### Where the sources still differ

A statement and the site carry the same claim number and member for a
claim, except where:

| Case | Why | What a store sees |
| --- | --- | --- |
| A statement prints another first name for the member than the site's | the member id is built from the first name | a second patient id; medrec asks `--person` on its first import |
| A pharmacy row's "Patient account number" is not a 17-digit number | the `rx` rule does not read it, and UHC does not document what another number means, so the row keeps its claim number rather than being merged on a guess | the statement's claim beside the site's |
| A statement prints a claim number the site no longer lists | the claim's id is its number | the statement's claim beside the site's |

## Resources

| Resource | From | Notes |
| --- | --- | --- |
| `Organization/uhc` | — | `type` `pay` |
| `Patient` | the header and "Claim detail for"; the site's profile | `identifier` the member id (`MB`); `name.given` the first name; `name.text` the full name when a statement is addressed to that member, or the site's first and last name. From the site also `name.family`, `birthDate`, `gender` |
| `Coverage` | the header; the site's medical plan coverage | `subscriberId`, `relationship` (EE or SUBSCRIBER self, SP or SPOUSE spouse, DP or DOMESTIC_PARTNER common, CH, CHILD or DEPENDENT child), `class` group number and name, `payor`. From the site also `period` (the plan period: the day the member's current coverage took effect, which is not the benefit period's start; see `benefitPeriod`) and `status` `cancelled` for a plan not `ACTIVE` |
| `ExplanationOfBenefit` | a "Claim detail for" block | below |
| `DocumentReference` | the statement PDF | `type.text` "Explanation of Benefits", `context.related` its claims, `context.period` the statement's service period. Every statement that carried one of the member's claims is exported, including one a later statement superseded |
| `DocumentReference` | a site document addressed to the member | `type.text` the site's document type (EOB, health statement, a letter), `category` "EOB" or that type, `date` its creation, `context.related` the exported claims its rows list. Every document addressed to the member is exported; the plan's general forms, addressed to no one, are not |
| `Binary` | the statement PDF | base64; the `DocumentReference` points at it with `content[].attachment.url` `Binary/<id>` |

### ExplanationOfBenefit

| Element | From |
| --- | --- |
| `identifier` (`uc`) | Claim number |
| `type` | the site's: `claim-type` `professional` (a medical claim whose lines carry no revenue code), `institutional` (a revenue code), `pharmacy` (RX), `oral` (DENTAL). A statement prints no claim type: `type.text` "Medical claim", or `pharmacy` for its pharmacy claims |
| `status` | `active`; `cancelled` for a pharmacy claim the site lists as reversed |
| `billablePeriod` | the first service line's date to the last |
| `created` | the statement date, or the site's processing date; when there is none, `_created` with the `data-absent-reason` extension (`unknown`) |
| `provider.display` | Provider; when none is printed, the `data-absent-reason` extension |
| `supportingInfo` | `info` with `valueReference` to the statement's `DocumentReference`, or to the latest site EOB listing the claim when the stream carries it; `patientaccountnumber` |
| `adjudication` | Status, or the site's `claimNetworkStatus`, as CARIN STU2's `benefitpaymentstatus` (`C4BBAdjudicationDiscriminator`) with reason `innetwork` / `outofnetwork` (`C4BBPayerAdjudicationStatus`) |
| `insurance` | the member's `Coverage` |
| `item[]` | each service line: `productOrService` (below), `revenue` (the site's revenue code, NUBC `https://www.nubc.org/CodeSystem/RevenueCodes`), `bodySite.text` (the site's tooth or tooth range), `servicedDate` or `servicedPeriod`, `noteNumber` for its codes, `net` the billed amount, `adjudication`, with a `not-counted` entry on a line the claim's totals leave out (below) |
| `total[]` | the "Total amount" row |
| `processNote[]` | "XX -- text", numbered in code order, `type` `print`: a statement's processing codes, or the site's line remark codes (`serviceRemarks`), which are the same codes |
| `benefitPeriod` | from the site, where the medical plan counts by calendar year (`benefitsPeriodIndicator` `CALENDAR_YEAR`): the calendar year of the claim's first service date, the same in every export. The site gives no dates, and it describes only the current plan, so uhc assumes earlier years counted the same way. None when the plan balances could not be read (the trailer's `skipped[]` says so), and none from a statement, which prints the plan year's number only |
| `benefitBalance[]` | the plan balances on the claim's statement: one per network (in, out) and unit (individual, family), term annual; `financial[]` deductible and out-of-pocket limit with `allowedMoney` (the maximum) and `usedMoney` (progress so far). None from the site, which shows the balances only as they stand today (`uhc accumulators`) |

`productOrService` is text from a statement. From the site it carries the
line's procedure code, its system by the code's shape, with the site's
description as `display` and `text`:

| Code | System |
| --- | --- |
| five digits, or four and `F`, `T` or `U` | CPT, `http://www.ama-assn.org/go/cpt` |
| `D` and four digits | CDT, `http://ada.org/cdt` |
| another letter and four digits | HCPCS Level II, `https://www.cms.gov/Medicare/Coding/HCPCSReleaseCodeSets` |
| one of these and one more letter or digit (`71046R`) | the system of its first five characters, coded as those five; a second coding carries the site's whole code, system `https://github.com/own-your-health/uhc-cli/fhir/site-procedure-code` |
| anything else | no coding; the code goes into `text` |

The site appends that character to some lines' codes and does not say what
it means. `claims get` gives it as the procedure's `suffix`, beside `site_code`. The site-code system is an
identifier like the tag and benefit-type systems; it was minted after the
repository moved, so it carries the `own-your-health` owner.

A pharmacy claim has one item, the drug's name as the site lists it
("Prescription drug" where it masks the name).

The nine amounts and their adjudication categories, each coding with its
code system's display text:

| Statement column | Site field (line; claim) | Category |
| --- | --- | --- |
| Amount billed | `chargedAmount`; `totalBilledAmount` | `submitted` (HL7) |
| Your plan discount / Saved | `providerWriteOff`; `healthPlanDiscount` | `discount` (CARIN) |
| Amount allowed | `allowedAmount`, else charged less the write-off; billed less the discount, where the discount leaves out the write-offs of lines the billed total omits (below). An `eligible` entry below zero is left out, not written; zero is written | `eligible` (HL7) |
| Plan paid | `planPaidAmount`; `healthPlanPay` | `benefit` (HL7) |
| Deductible | `deductible` | `deductible` (HL7) |
| Copay | `copay` | `copay` (HL7) |
| Coinsurance | `coinsurance`; `claimCoinsurance` | `coinsurance` (CARIN) |
| Not covered | `memberNotCoveredAmount`; `claimMemberNotCovered` | `noncovered` (CARIN) |
| You owe | `patientResponsibility` | `memberliability` (CARIN) |

A claim's totals can leave out a line the site keeps in the claim, while
`healthPlanDiscount` still counts the line's write-off. uhc finds such
lines from the amounts: the one set of lines the plan paid
nothing on whose charges and amounts owed are exactly what
`totalBilledAmount` and `patientResponsibility` are short of all the lines'.
None is marked when no set fits, when more than one does, when the lines'
plan paid differs from the claim's `healthPlanPay`, or when more than 16
lines are candidates. Each such line's
`item.adjudication` gains an entry with category code `not-counted` in
`https://github.com/own-your-health/uhc-cli/fhir/adjudication` and no
amount; its other amounts are the site's. Their write-offs are taken out of
the claim's discount, so the discount and the allowed amount are the
counted lines'.

The site's claims list gives one claim's totals as a summary whose
`other` is coinsurance and the not-covered amount together; `claims list`
reports it as coinsurance. The claim's detail, which `claims get` and the
export read, splits them.

The out-of-pocket limit has no HL7 benefit-type code, so it carries
`https://github.com/piekstra/uhc-cli/fhir/benefit-type|out-of-pocket-maximum`
and the text "Out-of-pocket limit".

## The stream

`records-stream/v1`, written by medical-common's `RecordsWriter`
(`pk-med-records`): a header with `method` `portal`, the organization
`{id: "uhc", name: "UnitedHealthcare", fhir_base: "https://member.uhc.com/myuhc", vendor: "uhc"}`
and the patient (`PatientContext`: `id`, `organization`, `label` when
`patients label` gave the member one, never its first name, `kind`
`member`, `member_role` `subscriber` or `dependent`, `relationship` `self`
for the subscriber); then the
resources in the order above; then `records-stream-end/v1` with counts. A
statement read with issues adds a `skipped[]` entry with code
`statement_issues` naming the document and the issue codes.

`--since D` exports what changed since D: the claims whose latest statement
is dated D or later (a claim with no statement date counts by its service
date), and the statements dated D or later that carry them. A claim
serviced in March and adjudicated on an April statement is in
`--since <April>`. `Patient`, `Organization` and `Coverage` are exported
whatever the date, and the header names them in `since_unbounded`.

From the site, `--since D` is the claims processed D or later, and only
their details are read, and the member's documents created D or later. A
claim the plan has not processed yet (`IN_PROCESS`) is not exported: its
id changes once it is processed. The trailer lists it in `skipped[]` with
code `in_process`. A read that fails partway (a session that expires)
ends the stream incomplete, the error in its trailer and the command's
exit code its own.
