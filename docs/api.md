# UnitedHealthcare: how claims and coverage are reached

UnitedHealthcare (UHC) offers a member three ways to their claims, coverage and
plan balances. Findings are from UHC's and Optum's public pages, read on
2026-11-12 without signing in. Quotes are UHC's or Optum's own wording.

| | Patient Access API (Optum FLEX) | Member site (myuhc.com) | Files the member downloads |
|---|---|---|---|
| What | CARIN Blue Button `ExplanationOfBenefit` and `Coverage`, US Core clinical resources, from 2016-01-01 | Claims, EOB PDFs, ID cards, coverage, deductible and out-of-pocket progress | EOB PDFs; the claims list as a spreadsheet |
| Who it serves | The plans under the CMS Patient Access rule; employer plans most likely not (below) | Every UHC member with a HealthSafe ID | Every member |
| Registration | An organization and an app at `flex.optum.com/portal`, each reviewed (5 business days each) | None | None |
| Terms | "Terms and Conditions (Patient Data API License)" | HealthSafe ID Terms of Use, which forbid software access ("scrape," "crawl," "spider") | Ordinary member use |
| `uhc` source | none (SMART standalone launch, if it ever serves the plan) | `--profile <NAME>`: a session signed in through `pk-cli-browser` (the member's decision and risk under the terms) | `--files <DIR>` (local files, no network) |

## Route

Decided: the member site. uhc signs in to myuhc.com in a browser as the
member and reads the site's own data, at the member's risk under its Terms
of Use. It serves employer plans, which the evidence below says the Patient
Access API does not: that API serves Medicare, Medicaid, CHIP and ACA
marketplace members.

The files source (v0.1.0) stays: it reads the EOB statement PDFs the member
downloads from the site (layout below) and never contacts UHC.

## Patient Access API (Optum FHIR Layer EXchange, "FLEX")

### Who it serves

UHC's own pages do not name the plan types. They say:

- "The patient access API lets UnitedHealthcare members securely access their
  health data through applications of their choice. ... it meets CMS
  interoperability requirements" (`uhc.com/legal/interoperability-apis`).
- "The Interoperability and Patient Access rule (CMS-9115-F) ... makes it easier
  for you to access and share your health data. ... This shared data is found
  with certain insurance plans." (`uhc.com/legal/share-my-health-data-notice`)
- "UnitedHealthcare gives eligible members secure ways to view certain health
  information using third-party apps" (UHC's IAL2 member guide, a PDF linked
  from the patient access page).

CMS-9115-F binds Medicare Advantage, Medicaid, CHIP and federally facilitated
marketplace plans; employer-sponsored plans are outside it. One health-data
aggregator with production access to this API publishes its supported lines of
business per payer (Flexpa's endpoint directory, entry "United Healthcare",
updated 2026-11-06): Patient Access API "Medicaid, Medicare, ACA (On Exchange),
ACA (Off Exchange), CHIP"; employer plans only through its own member-portal
integration, and only "Fully Funded". It is cited here as evidence only; the
family's rules forbid using aggregators.

Conclusion: an employer-plan member most likely gets no claims through this
API. Only a signed-in attempt with a registered production app settles it.

### Endpoints (fetched unauthenticated, 2026-11-12)

| Purpose | URL |
|---|---|
| FHIR base, production | `https://flex.optum.com/fhir/hsid/R4/` |
| FHIR base, sandbox | `https://flex.optum.com/fhir/sandbox/R4/` |
| SMART configuration | `<base>.well-known/smart-configuration` |
| Authorize | `https://flex.optum.com/authz/<payer>/oauth/authorize` |
| Token | `https://flex.optum.com/authz/<payer>/oauth/token` |
| OpenID configuration | `https://flex.optum.com/authz/<payer>/.well-known/openid-configuration` |
| Public directory (no auth) | `https://flex.optum.com/fhirpublic/R4/` |
| Developer and member portal | `https://flex.optum.com/portal/homepage` |

`<payer>` is `hsid` for all four UHC payers in UHC's table (UnitedHealthcare,
Sierra Health and Life, Rocky Mountain, People's Health) and `sandbox` for the
sandbox. The capability statement reports software `optum-flex-server-default`
4.6.1.7, FHIR 4.0.1.

### Authorization

From the SMART configuration: `launch-standalone`,
`client-confidential-symmetric`, `context-standalone-patient`,
`permission-patient`; `code_challenge_methods_supported: ["S256"]`; grants
`authorization_code` and `client_credentials`. The OpenID configuration lists
`token_endpoint_auth_methods_supported: ["client_secret_post", "private_key_jwt"]`.

UHC's page:

- "A registered application is given a client ID and a client secret. The
  secret should only be used if it can be kept confidential ... For insecure
  implementations, such as mobile apps, PKCE (Proof Key for Code Exchange) is
  available." The token request carries either `client_secret` or
  `code_verifier`.
- Redirects: "The redirect_uri supports any URI protocol ... The https://
  format ... is required for all applications in the production environment
  unless the application is using the Mobile OAuth method for handling
  callbacks." The custom scheme form is reverse-domain
  (`Top-level.domain(TLD).domain-name[.sub-domain][.app_name]`). A loopback
  `http://127.0.0.1` redirect is therefore not accepted in production; a native
  CLI needs either a custom scheme handler or an https page that hands the code
  to a loopback listener.
- Scopes, which "must be explicitly declared; wildcards are not supported":
  `patient/Condition.read`, `patient/Coverage.read`, `patient/Encounter.read`,
  `patient/ExplanationOfBenefit.read`, `patient/Immunization.read`,
  `patient/MedicationDispense.read`, `patient/MedicationRequest.read`,
  `patient/Observation.read`, `patient/Patient.read`,
  `patient/Procedure.read`. The member can decline any scope on the consent
  screen.
- The token response carries `patient`, `expires_in` and `refresh_token`.
  "The access token will be short-lived, typically 5 minutes or less." On
  refresh, "A new refresh token will not be issued": refresh tokens do not
  rotate.
- Sensitive data (substance use disorder, HIV/AIDS, psychotherapy,
  reproductive health, communicable disease) is released only with the
  member's separate permission (share-my-health-data notice).
- Release 4.6, dated 2026-09-22, added a path for apps that verified identity at
  IAL2/AAL2 through CLEAR; the HealthSafe ID sign-in remains the standard path.

### Registration

1. Create a One Healthcare ID; sign in to the App Owner portal at
   `flex.optum.com/portal`.
2. Register an organization: "An organization must register as a user ... Our
   security and compliance team will review your organization registration and
   approve or deny your ability to register applications." Estimated 5
   business days.
3. Register a sandbox app: immediate, "mock data".
4. Register a production app: "Production application requests for Patient
   Access APIs will require review from our security and compliance team".
   Estimated 5 business days.
5. A public privacy policy URL is required for the organization and the app.

The terms shown on the portal are "Terms and Conditions ( Patient Data API
License )". Points a household should know: the recipient obtains and keeps
member consent; the app's privacy policy and terms of use "must be made
publicly available"; breach notification under the FTC Health Breach
Notification Rule; UHG may limit or revoke access at its discretion; "UHG
liability capped at $50"; the recipient indemnifies UHG; "Recipient waives
right to participate in class action lawsuits"; "Mandatory binding arbitration
required"; New York law; "Continued use of the site is the acknowledgement and
acceptance of changes ... – no notifications will be made."

Support: `flexvendorsupport@optum.com`, Monday to Friday 9am to 4pm CST.

### Data

- `ExplanationOfBenefit` (CARIN BB 2.0.0; institutional, professional, oral,
  vision, pharmacy): "Returns records dated back to 2016-01-01, inclusive."
  Search parameters `_id`, `_count`, `_lastUpdated`, `identifier`,
  `service-date`, `type`, `patient`, `use`; paging by `_getpagesoffset`.
- `Coverage` (CARIN BB): "the coverage data that was effective as of the date of
  service of the claim." Search `_id`, `_count`, `_lastUpdated`, `patient`.
- Claims appear "1 business day from adjudication".
- Implementation guides: US Core 6.1.0, CARIN BB 2.0.0, PDex 1.0.0, Plan-Net
  1.1.0, US Drug Formulary 1.0.1.

### Dependents

A token carries one `patient`. UHC does not document whether a subscriber's
sign-in can authorize a dependent's record. Across CMS-9115 payers the common
pattern is one authorization per member, with the signed-in person choosing
self or a dependent on the consent screen.

## Member site (myuhc.com)

Mapped 2026-11-17 by driving a signed-in session: page loads in the
owner's own window, then the same requests replayed with the session's
cookies. Every read below is a GraphQL `query` or a `GET`; uhc never sends
a mutation.

- Entry `https://www.myuhc.com/` redirects to `https://member.uhc.com/myuhc`.
  Sign-in is HealthSafe ID at `identity.healthsafe-id.com/oneapp`, an Angular
  app on CloudFront. Its public bundle carries ThreatMetrix device profiling
  and reCAPTCHA. uhc never automates the sign-in: the owner signs in on the
  site's own page, in the Chrome window `uhc auth login --browser` opens.
- Terms: the HealthSafe ID Terms of Use (effective September 28, 2022), linked
  from the sign-in page. Under "Restrictions on Use of Online Services ... You
  Will Not": "Copy, modify or harvest data, Content, or materials from the
  Online Services"; "Share any password with any third parties"; "use software
  or other means to access, "scrape," "crawl," or "spider," any web pages or
  other services from the Online Services. If you are blocked ... you agree not
  to implement any measures to circumvent such blocking"; "Attempt to reverse
  engineer any of the software used to provide the Online Services." The
  license is "personal, non-commercial ... to view the Content". "If you breach
  any of these Terms, your license to the Online Services and its Content
  terminates immediately." Disputes go to binding arbitration with a class
  action waiver; disputes "about the existence or extent of your health
  benefits or coverage" are excluded from arbitration. The Terms of Use the
  member site's footer loads (`myoptum.optum.com/content/optum-dashboard/en/footer/uhc/terms.html`)
  say the same: "not to use manual or automated software, devices, scripts,
  robots, backdoors or other means or processes to access ... "scrape,"
  "crawl" or "spider" any web pages". Using uhc against the member site is
  the member's decision and risk under these terms (README, "The site's
  terms, in plain words").

### The session

| | |
|---|---|
| Before sign-in | `member.uhc.com` sets visitor cookies only: `AWSALBTG`, `AWSALBTGCORS`, `X-DashCafe-Canary` |
| Signed in | about 50 cookies on `member.uhc.com` and `.uhc.com`; `rally-token`, `RS_MU`, `BARN_SP`, `FD_MU` and `JSESSIONID` are `HttpOnly`. `rally-token` is the capture's readiness signal |
| Lifetime | every `member.uhc.com` session cookie is a browser-session cookie (no expiry), so a Chrome restart signs the profile out; the kept profile does not stay signed in. Measured on 2026-11-17 with one session read by `sessionCheck` alone at growing gaps: `200` after 5, 10, 20 and 40 minutes idle (75 minutes after sign-in), `401` after 80 minutes idle (155 minutes after sign-in). So the session survives at least 40 minutes idle and lasts under 155 minutes unattended; whether an idle limit (40 to 80 minutes) or an absolute one (75 to 155 minutes) ended it is not yet known. Expect one sign-in per sitting: a session from the morning is gone by the afternoon |
| Signed out | a GraphQL read answers `401 application/json`: `{"meta":{"success":false,...,"errors":[{"error":"Credentials missing.",...}]}}` |
| Session check | `POST /ce-claims-aggregate-api/graphql?q=sessionCheck` with `query sessionCheck { spendingOverviews { __typename } }`: `200` with `data.spendingOverviews` when signed in, `401` otherwise |

### Reads

All on `https://member.uhc.com`, `POST` with a JSON body
`{"operationName", "variables", "query"}` unless marked `GET`.

| What | Request | Answer |
|---|---|---|
| Members | `/ce-claims-aggregate-api/graphql?q=getAllClaims`, `getAllClaims(startDate, stopDate)` | `allClaimsAggregate.profileV1`: `currentUser` and `dependents[]` (names, `dependentSeqNum`, `planCoverages[]` with `memberId`, `groupNumber`, `coverageType`), `claimCoverageTypes` |
| Claims | `/ce-claims-aggregate-api/graphql?page=allClaims&cache=off`, `allClaims(startDate, stopDate)` (ISO dates; the page asks for 18 months) | `allClaimsAggregate.healthcareClaims.claims[]`: `claimType` (`MEDICAL`, `RX`, `DENTAL`), `claimKey`, `claimId`, `claimStatus`, `serviceDate`, `lastServiceDate`, `processedDate`, `providerName`, `eobDocument`, `serviceRecipient` (`firstName`, `lastName`, `dependentSeqNbr`), `balance` (`totalBilledAmount`, `healthPlanDiscount`, `healthPlanPays`, `deductible`, `copay`, `patientResponsibility`, `youMayOweAmount`, each `{value, iso4217}`) |
| FSA claims | same endpoint, `allFinancialClaims(startDate, stopDate, accountType: "FSAHC")` | `financialClaims[]` |
| One claim | `/ce-claims-aggregate-api/graphql?q=getClaimDetails`, `getClaimDetails(claimKey, claimType)` | `claimDetailsAggregate.healthCareClaimDetails`: `claimDates`, `claimNetworkStatus`, `providerId` (`tin`), `claimHistory[]`, `serviceLines[]` with `serviceDate`, `procedures[]` and `serviceInfo` (`code`, `description`), `serviceRemarks[]` (`code`, `description`) and `serviceAdjudication` (`chargedAmount`, `planPaidAmount`, `deductible`, `copay`, `coinsurance`, `memberNotCoveredAmount`, `patientResponsibility`, `providerWriteOff`) |
| Plan balances | `/ce-claims-aggregate-api/graphql?q=getSpendingOverview`, no variables | `spendingOverviews.accumulators[]` per `depSeqNbr`: `accumulatorSummaries[].benefits[]` with `coverageType`, `benefitsPeriodIndicator` and `accumulators.inNetwork[]`, `outOfNetwork[]`, `tier1[]` (`accumulatorType`, `type`, `amount`); `benefits[].benefitsResponses[]` with the plan's `maxes` |
| Spending by month | `/ce-claims-aggregate-api/graphql?page=spendingOverviews`, `GetSpendingCostsV1(startDate, endDate)` | `costs.spendingCosts[].memberData[]` by category and month |
| Documents | `/rest/oneportal/web/documents-aggregate-api/v1/graphql`, `GetAllDocumentTypes(processedStartDate, processedStopDate, mnrConsumerDetails: {planProfiles: []})` (the page asks for 3 months) | `multiDocTypeListSummary.documents[]`: `documentId`, `documentType` ("Explanation of Benefits", "Health Statement", "Prior Authorization Letters", ...), `documentClass`, `claimIDs[]`, `claimKey`, `dateCreated`, `memberName`; `errors[]` per document type |
| A document | `GET /oneportal/view/documents-aggregate-api/v1/v2/doc360/content/id/<uuid>?documentClass=...` | `application/pdf`. Several statements share one `<uuid>` and differ only in the query |
| Plan benefits | `/benefits/eni/graphql?traceName=...`: `globalQuery`, `fastQuery` (the benefit catalog), `summaryBenefits(benefitIds, benefitPlanId)`, `benefitDetails`, `additionalBenefits` | benefit text as rich-text nodes; the certificate of coverage is a PDF linked from the coverage page |
| ID card | `GET /rest/oneportal/web/integrated?components=idCard...&pageId=DIGITALIDCARD` | the card's sections, images included (about 330 KB) |

### What uhc reads

`src/client.rs` sends four queries (`src/site/queries/*.graphql`) and one
`GET`, one at a time, at least a second apart, each with the stored
`Cookie` header and the browser's user agent. The variables are dates
only; every other id (`claimKey`, `claimType`, `documentId`,
`documentClass`) comes from an answer.

| uhc's read | Request | Variables |
|---|---|---|
| `claims` | `POST /ce-claims-aggregate-api/graphql?q=claims&cache=off`, `allClaimsAggregate { profileV1, healthcareClaims }` | `startDate` 2016-01-01, `stopDate` today |
| `claimDetails` | `POST /ce-claims-aggregate-api/graphql?q=claimDetails`, `claimDetailsAggregate(claimKey, claimType)` | the claim's latest `claimKey` and its `claimType` |
| `spending` | `POST /ce-claims-aggregate-api/graphql?q=spending`, `spendingOverviews { profileV1, benefits, accumulators }` | none |
| `documents` | `POST /rest/oneportal/web/documents-aggregate-api/v1/graphql`, `multiDocTypeListSummary(processedStartDate, processedStopDate, mnrConsumerDetails: {planProfiles: []})`, with `XSRF-TOKEN` and `RP-Token-Suffix: MU` | `processedStartDate` 2016-01-01, `processedStopDate` today |
| a document | `GET /oneportal/view/documents-aggregate-api/v1/v2/doc360/content/id/<documentId>?documentClass=<documentClass>`, `Accept: application/pdf,application/json` | |

| Command | Reads |
|---|---|
| `patients list` | claims, documents |
| `claims list` | claims |
| `claims get` | claims, documents, the claim's detail |
| `accumulators` | spending |
| `documents list` | claims, documents |
| `documents download` | claims, documents, each PDF (`--all` skips a file already saved under its name) |
| `records export` | claims, documents, spending (for the benefit period only: when it fails, the claims go out without one), each of the member's claims' details (with `--since`, those processed since), each of the member's documents (with `--since`, created since) |

Measured 2026-11-17 on one household's account:

| Window | One answer |
|---|---|
| Claims, 2016-01-01 to today | every claim: back to 2022 (dental) and 2023 (medical); no paging |
| Documents, 2016-01-01 to today | every document, created from 2022 on, one row per document and claim; no paging |

How the answers read:

- Members: `profileV1.currentUser` and `dependents[]`, one family member
  id for every member and both coverages (medical and dental);
  `relationshipType` `SUBSCRIBER`, `SPOUSE`, `DEPENDENT`; `gender` `MALE`
  or `FEMALE`; the profile leaves null fields out (`groupName`).
- A medical claim's `claimId` is `<base>-<suffix>` or `<base>`, with the
  suffix in `claimIdSuffix` too; `<base><suffix>` is the claim number a
  statement prints. A pharmacy claim's is 18 digits; its first 17 are a
  statement's "Patient account number" for it (one sample). A dental
  claim's is 12 digits without a suffix; no dental statement has been
  compared.
- A reprocessed claim is listed once per version, with the same id and a
  later `processedDate`. A reversed pharmacy claim is listed again with
  negative amounts. A claim `IN_PROCESS` has no suffix, no `processedDate`,
  no network status and most of its balance null.
- The summary's `balance.other` is coinsurance and the not-covered amount
  together; the detail's `claimCoinsurance` and `claimMemberNotCovered`
  split it. In the summary, billed = discount + plan pays + patient
  responsibility.
- A detail line: `chargedAmount` = `providerWriteOff` + `planPaidAmount`
  + `patientResponsibility`, and that = deductible + copay + coinsurance
  + `memberNotCoveredAmount`. A medical line's `allowedAmount` is null; a
  dental line's is set, with `toothNumber` a tooth or a range (`01-32`).
  `serviceRemarks[].code` are the processing codes a statement prints
  (`D1`, `UG`).
- Plan balances: `benefits[].benefitsResponses[].benefits[].maxes` holds
  the maximums by `type` (`INDIVIDUAL_DEDUCTIBLE`, `INDIVIDUAL_OOP`,
  `FAMILY_DEDUCTIBLE`, `FAMILY_OOP`), `accumulators[].accumulatorSummaries`
  the progress by `accumulatorType` and `type`, both per `depSeqNbr`.
  `benefitsPeriodIndicator` on each says how the plan counts: medical
  `CALENDAR_YEAR`, the only value seen, and `null` for dental and
  pharmacy (`RX`). Neither carries a period's dates. Dental has annual and lifetime maximums instead
  (`uhc accumulators` reports medical only); `tier1` and pharmacy are null.
- Documents: one member, class and creation date per `documentId`, and one
  row per claim it covers (`claimKey`). `memberName` is `FIRST LAST`. The
  plan's general forms ("Other Forms": proof of coverage and the like) have
  no member and no date, and a `documentClass` that is a title; uhc leaves
  them out. A PDF's bytes differ on each download.
- `eobDocument` on a claim names no document the documents read lists.

### Traps

- A coverage's `planPeriod.startDate` is the day the member's current
  coverage took effect, not the plan year's start: a change to the
  household (a member added) starts a new period mid-year, and its
  `endDate` can be `9999-12-31`. The plan year is in the balances
  answer's `benefitsPeriodIndicator`, as a kind of period with no dates.
- A request without a browser's `User-Agent` gets `403 text/html` from the
  edge, signed in or not. uhc sends the user agent captured with the
  session; `auth login --stdin` takes it as `--user-agent`.
- The signed-in pages have no sign-out link a page script can see: the
  site header is a web component. Readiness is a cookie, not the DOM.
- The GraphQL endpoints answer queries the site never sends, such as the
  one-field session check.
- The documents service answers `403 application/json`
  (`{"timestamp", "status": 403, "error": "Forbidden", "path"}`) without
  `XSRF-TOKEN`, the first `!`-separated part of the URL-decoded `FH_MU`
  cookie, and `RP-Token-Suffix: MU`. The claims service needs neither.
- A request without a session gets `401 application/json`
  (`{"meta": {"success": false, "errors": [{"error": "Credentials
  missing."}]}}`) from the claims service; a non-browser user agent gets
  the edge's `403` page first.
- Answers set `AWSALBTG`, `AWSALBTGCORS` and `__cf_bm`; uhc keeps what they
  set and writes the session back once a command has succeeded.
- `benefit-details?benefitName=` for a benefit the plan does not list loads
  no `benefitDetails` at all. Home health care and private-duty nursing
  are only in the certificate of coverage, not the benefit pages.
- `uhc.com`'s legal pages answer `403` to fetch tools.
- A claim's `balance.totalBilledAmount` can leave out a service line the
  provider wrote off in full (charged equals `providerWriteOff`, nothing
  else) while `balance.healthPlanDiscount` still counts its write-off, so
  billed less the discount is negative. In other claims a line of that
  shape is in both. uhc takes the write-off out of the discount only when
  the lines' charges exceed the billed total by exactly such lines'
  charges (`portal::left_out_write_off`).
- The site's own claim page (`GetHealthcareClaimDetails`) asks for more
  than uhc does. On each service line it adds `lineAdjudicationStatus`,
  `deniedClaimInfo` (`explanation`, `category`, `cta`), `billingCode`,
  `planCode` and `serviceAdjudication.planCostShare`. On the claim it adds
  `claimDisposition`, `isReprocessedClaim`, `balance.totalContractAmount`,
  `balance.claimAdjustmentAmount` (shown in place of `healthPlanPay` when
  present) and `claimHistory[]` (past claim ids, keys, dates and statuses,
  nothing per line). Their values are unread.
- `procedures[].code` is sometimes a CPT or HCPCS code with one more letter
  or digit (`710467`), and the site gives no field that says what the
  character means. Its claim page shows `procedures[0].code` only on dental
  lines; on the others it shows `serviceInfo.code` as the "Billing code"
  and `planCode` as the "Plan code". Neither explains the character: on a
  line that carries one, `serviceInfo.code` and `providedServiceInfo.code`
  are the same six characters as `procedures[].code`, and
  `serviceLines[].planCode` is null. uhc does not query `planCode`, and
  reads `serviceInfo.code` only when `procedures[]` has no usable code.
  It splits the character off (`fhir::split_procedure`) and keeps it.

## EOB statement layout (UHC's public sample)

UHC's explainer `uhc.com/content/dam/uhcdotcom/en/general/understanding-your-eob-flier.pdf`
shows an employer-plan EOB ("Group name", "Group number", relationship "EE"):

- Header: member/patient, member ID, relationship, group name and number.
- "Claim summary": the service date range, "Provider billed", "Your total amount
  owed".
- "Plan balances", "As of MM/DD/YYYY for plan year YYYY", one table per person
  and one for "FAMILY": rows "Network" and "Out-of-Network", each with
  "Deductible" and "Out-of-pocket limit", columns "Maximum amount", "Progress so
  far", "Remaining amount" (a met limit reads "Met").
- Then claim details per claim and line, and definitions.

These balances are the accumulators `uhc accumulators` reports when the source
is an EOB file.

## Traps

- `uhc.com` returns 403 to `curl` for its PDFs (bot protection); fetch them in a
  browser.
- UHC's authorize example writes `Host: https://flex.optum.com/authz/[payer]/oauth/token`;
  the authorize endpoint is `.../oauth/authorize`, as the SMART configuration
  says.
- The SMART configuration's `introspection_endpoint` points at a `jwks.json`,
  and the OpenID configuration's `jwks_uri` is the relative `/jwks`.
- The SMART configuration does not list `client-public`, although UHC's page
  offers PKCE for public clients.
- Access tokens last about 5 minutes: refresh before each batch of requests,
  not once per run.
