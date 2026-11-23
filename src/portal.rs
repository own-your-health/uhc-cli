//! The member site as a source: its answers read into the model the
//! statements fill (`model::Member`, `ClaimRecord`, `Balances`), with the
//! same ids, so one claim or member is one record to medrec whichever source
//! it came from (docs/fhir.md § Identity).
//!
//! - A member is `patient_id(member id, first name)`, as on a statement.
//! - A medical or dental claim is `c` and its claim number: the `claimId`
//!   without its `-NN` part, then the `claimIdSuffix` (a statement prints
//!   `SYNC00000006` for `SYNC000000-06` / `06`). A pharmacy claim is
//!   `pharmacy_claim_id`'s `rx` and the first 17 digits.
//! - The site lists a reprocessed claim once per version, and a reversed
//!   pharmacy claim again with negative amounts: one record each, the
//!   latest version's, a reversal cancelled with the original's amounts.
//! - Amounts are strings (`"123.45"`), or null where the site has none.
//!
//! [`Site`] makes the reads through [`Client`] and writes the session back
//! once the command has read what it needed, when the answers updated it.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use pk_cli_config::profile::ProfileName;
use pk_cli_core::{dates, CliError};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cents::Cents;
use crate::client::{self, Client};
use crate::eob::{Amounts, Claim, ClaimLine, Coded, Limit, Network};
use crate::model::{
    family_id, patient_id, pharmacy_claim_id, sanitize, title, Balances, ClaimKind, ClaimRecord, Member, Origin,
    SiteDocument, SiteMember, ORGANIZATION, ORGANIZATION_NAME,
};
use crate::session::{Session, SessionStore};

/// The first day the claims and documents reads ask from. The site answers
/// a window this wide in one answer (docs/api.md).
pub const FIRST_DAY: &str = "2016-01-01";

// ------------------------------------------------------------- answers

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct ProfileV1 {
    current_user: Option<RawMember>,
    dependents: Option<Vec<RawMember>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct RawMember {
    user_info: Option<UserInfo>,
    dependent_seq_num: Option<String>,
    relationship_type: Option<String>,
    plan_coverages: Option<Vec<PlanCoverage>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct UserInfo {
    first_name: Option<String>,
    last_name: Option<String>,
    dob: Option<String>,
    gender: Option<String>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct PlanCoverage {
    coverage_type: Option<String>,
    group_number: Option<String>,
    group_name: Option<String>,
    member_id: Option<IdBox>,
    plan_period: Option<PlanPeriod>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct IdBox {
    id: Option<String>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct PlanPeriod {
    start_date: Option<String>,
    end_date: Option<String>,
    status: Option<String>,
}

/// `{value, iso4217}`; the value a string or a number.
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(default)]
struct Money {
    value: Option<Value>,
}

impl Money {
    fn cents(m: &Option<Money>) -> Option<Cents> {
        m.as_ref().and_then(|m| m.value.as_ref()).and_then(amount)
    }
}

/// A site amount as cents: `"123.45"`, `"-5"`, `12.5` or `"1,234.50"`.
/// More than two decimals, or anything that is not a number, is `None`.
pub fn amount(v: &Value) -> Option<Cents> {
    let text = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let (negative, rest) = match text.strip_prefix('-') {
        Some(r) => (true, r.trim()),
        None => (false, text.as_str()),
    };
    let rest = rest.strip_prefix('$').unwrap_or(rest).replace(',', "");
    let (whole, frac) = rest.split_once('.').unwrap_or((&rest, ""));
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) || frac.len() > 2 {
        return None;
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let units: i64 = whole.parse().ok()?;
    let frac: i64 = format!("{frac:0<2}").parse().ok()?;
    let cents = units.checked_mul(100)?.checked_add(frac)?;
    Some(Cents(if negative { -cents } else { cents }))
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct ClaimsData {
    all_claims_aggregate: Option<ClaimsAggregate>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct ClaimsAggregate {
    profile_v1: Option<ProfileV1>,
    healthcare_claims: Option<HealthcareClaims>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct HealthcareClaims {
    claims: Option<Vec<RawClaim>>,
}

#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "camelCase", default)]
struct RawClaim {
    claim_id: Option<String>,
    claim_id_suffix: Option<String>,
    claim_key: Option<String>,
    claim_type: Option<String>,
    claim_status: Option<String>,
    service_recipient: Option<Recipient>,
    provider_name: Option<String>,
    service_date: Option<String>,
    last_service_date: Option<String>,
    processed_date: Option<String>,
    claim_network_status: Option<String>,
    medication_name: Option<String>,
    balance: Option<SummaryBalance>,
}

#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Recipient {
    first_name: Option<String>,
    dependent_seq_nbr: Option<String>,
}

#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "camelCase", default)]
struct SummaryBalance {
    total_billed_amount: Option<Money>,
    health_plan_discount: Option<Money>,
    health_plan_pays: Option<Money>,
    deductible: Option<Money>,
    copay: Option<Money>,
    /// Coinsurance and what the plan does not cover, together.
    other: Option<Money>,
    patient_responsibility: Option<Money>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct DetailData {
    claim_details_aggregate: Option<DetailAggregate>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct DetailAggregate {
    health_care_claim_details: Option<RawDetail>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct RawDetail {
    claim_network_status: Option<String>,
    provider_name: Option<String>,
    claim_dates: Option<ClaimDates>,
    balance: Option<DetailBalance>,
    service_lines: Option<Vec<ServiceLine>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct ClaimDates {
    date_processed: Option<String>,
    service_start_date: Option<String>,
    last_service_date: Option<String>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct DetailBalance {
    total_billed_amount: Option<Money>,
    health_plan_discount: Option<Money>,
    health_plan_pay: Option<Money>,
    deductible: Option<Money>,
    copay: Option<Money>,
    claim_coinsurance: Option<Money>,
    claim_member_not_covered: Option<Money>,
    patient_responsibility: Option<Money>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct ServiceLine {
    service_date: Option<String>,
    last_service_date: Option<String>,
    procedures: Option<Vec<RawCode>>,
    revenues: Option<Vec<RawCode>>,
    service_info: Option<RawCode>,
    provided_service_info: Option<RawCode>,
    service_remarks: Option<Vec<RawCode>>,
    tooth_number: Option<String>,
    service_adjudication: Option<Adjudication>,
}

#[derive(Deserialize, Default, Debug, Clone)]
#[serde(default)]
struct RawCode {
    code: Option<String>,
    description: Option<String>,
}

impl RawCode {
    fn coded(&self) -> Option<Coded> {
        let code = self.code.as_deref().map(str::trim).filter(|c| !c.is_empty())?;
        Some(Coded {
            code: code.to_string(),
            display: text(&self.description),
        })
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct Adjudication {
    charged_amount: Option<Money>,
    allowed_amount: Option<Money>,
    provider_write_off: Option<Money>,
    plan_paid_amount: Option<Money>,
    deductible: Option<Money>,
    copay: Option<Money>,
    coinsurance: Option<Money>,
    member_not_covered_amount: Option<Money>,
    patient_responsibility: Option<Money>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct SpendingData {
    spending_overviews: Option<SpendingOverviews>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct SpendingOverviews {
    profile_v1: Option<ProfileV1>,
    benefits: Option<Vec<DepBenefits>>,
    accumulators: Option<Vec<DepAccumulators>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct DepBenefits {
    dep_seq_nbr: Option<String>,
    benefits_responses: Option<Vec<BenefitsResponse>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct BenefitsResponse {
    benefits: Option<Vec<Benefit>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct Benefit {
    coverage_type: Option<String>,
    maxes: Option<ByNetwork>,
    accumulators: Option<ByNetwork>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct ByNetwork {
    in_network: Option<Vec<Accumulator>>,
    out_of_network: Option<Vec<Accumulator>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct Accumulator {
    #[serde(rename = "type")]
    kind: Option<String>,
    amount: Option<Money>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct DepAccumulators {
    dep_seq_nbr: Option<String>,
    accumulator_summaries: Option<Vec<AccumulatorSummary>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct AccumulatorSummary {
    benefits: Option<Vec<Benefit>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct DocumentsData {
    multi_doc_type_list_summary: Option<DocumentList>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct DocumentList {
    documents: Option<Vec<RawDocument>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
struct RawDocument {
    member_name: Option<String>,
    document_type: Option<String>,
    document_class: Option<String>,
    document_id: Option<String>,
    document_name: Option<String>,
    date_created: Option<String>,
    claim_key: Option<String>,
}

fn list<T>(v: &Option<Vec<T>>) -> &[T] {
    v.as_deref().unwrap_or(&[])
}

/// A trimmed, non-empty string.
fn text(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// An ISO date the site gave (`2026-08-15`, or the date of a timestamp).
fn date(s: &Option<String>) -> Option<String> {
    let s = s.as_deref()?.trim();
    let day = s.get(..10)?;
    dates::parse_iso(day).ok().map(dates::fmt_iso)
}

fn decode<T: serde::de::DeserializeOwned>(data: Value, what: &str) -> Result<T, CliError> {
    serde_json::from_value(data).map_err(|e| {
        CliError::Upstream(format!(
            "the member site's {what} answer is not the shape uhc reads: {e}"
        ))
    })
}

// ------------------------------------------------------------- members

/// The plan's members: the subscriber first, then the dependents, in the
/// site's order.
fn members(profile: &ProfileV1) -> Vec<Member> {
    profile
        .current_user
        .iter()
        .chain(list(&profile.dependents))
        .filter_map(member)
        .collect()
}

fn member(raw: &RawMember) -> Option<Member> {
    let info = raw.user_info.as_ref()?;
    let first = text(&info.first_name)?;
    let coverages = list(&raw.plan_coverages);
    let medical = coverages
        .iter()
        .find(|c| {
            c.coverage_type
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("MEDICAL"))
        })
        .or_else(|| coverages.first())?;
    let member_id = crate::eob::canonical_member_id(&medical.member_id.as_ref().and_then(|m| text(&m.id))?);
    let relationship_code = text(&raw.relationship_type);
    let period = medical.plan_period.as_ref();
    let gender = text(&info.gender).map(|g| match g.to_ascii_uppercase().as_str() {
        "MALE" | "M" => "male".to_string(),
        "FEMALE" | "F" => "female".to_string(),
        "OTHER" => "other".to_string(),
        _ => "unknown".to_string(),
    });
    Some(Member {
        id: patient_id(&member_id, &first),
        organization: ORGANIZATION,
        organization_name: ORGANIZATION_NAME,
        label: title(&first),
        relationship: relationship_code
            .as_deref()
            .and_then(crate::model::relationship)
            .map(str::to_string),
        relationship_code,
        member_id,
        group_number: text(&medical.group_number),
        group_name: text(&medical.group_name),
        claims: 0,
        statements: 0,
        latest_statement: None,
        origin: Origin::Site,
        site: Some(SiteMember {
            dep_seq: text(&raw.dependent_seq_num).unwrap_or_default(),
            first_name: first,
            last_name: text(&info.last_name),
            birth_date: date(&info.dob),
            gender,
            plan_start: period.and_then(|p| date(&p.start_date)),
            plan_end: period.and_then(|p| date(&p.end_date)),
            plan_status: period.and_then(|p| text(&p.status)),
        }),
        grant_label: None,
    })
}

fn by_seq<'a>(members: &'a [Member], seq: Option<&str>) -> Option<&'a Member> {
    let seq = seq?.trim();
    members
        .iter()
        .find(|m| m.site.as_ref().is_some_and(|s| s.dep_seq == seq))
}

fn by_first_name<'a>(members: &'a [Member], first: &str) -> Option<&'a Member> {
    let mut hits = members.iter().filter(|m| m.label.eq_ignore_ascii_case(first.trim()));
    let one = hits.next()?;
    hits.next().is_none().then_some(one)
}

// -------------------------------------------------------------- claims

/// What reads a claim's detail: its key and type, from the claims answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailKey {
    pub claim_key: String,
    pub claim_type: String,
}

/// The members and claims of one claims answer. `keys` is each claim id's
/// detail key (its latest version's); `all_keys` every version's, which the
/// documents list a claim under.
#[derive(Debug, Default)]
pub struct Household {
    pub members: Vec<Member>,
    pub claims: Vec<ClaimRecord>,
    pub keys: HashMap<String, DetailKey>,
    pub all_keys: HashMap<String, Vec<String>>,
}

impl Household {
    /// The subscriber's member id, for a document or claim naming no one the
    /// profile lists.
    fn member_id(&self) -> Option<&str> {
        self.members.first().map(|m| m.member_id.as_str())
    }
}

/// A claim's id and number: see the module docs.
pub fn claim_identity(kind: &str, claim_id: &str, suffix: Option<&str>) -> (String, String) {
    let claim_id = claim_id.trim();
    if kind.eq_ignore_ascii_case("RX") {
        if let Some(rx) = pharmacy_claim_id(claim_id) {
            return (rx, claim_id.to_string());
        }
    }
    let suffix = suffix.map(str::trim).filter(|s| !s.is_empty());
    let number = match (claim_id.split_once('-'), suffix) {
        (Some((base, _)), Some(s)) => format!("{base}{s}"),
        (Some((base, tail)), None) => format!("{base}{tail}"),
        (None, Some(s)) => format!("{claim_id}{s}"),
        (None, None) => claim_id.to_string(),
    };
    (format!("c{}", sanitize(&number)), number)
}

fn network(s: &Option<String>) -> Option<Network> {
    match s.as_deref()?.to_ascii_uppercase().as_str() {
        "IN_NETWORK" => Some(Network::In),
        "OUT_OF_NETWORK" => Some(Network::Out),
        _ => None,
    }
}

fn kind_of(claim_type: &str) -> ClaimKind {
    match claim_type.to_ascii_uppercase().as_str() {
        "RX" => ClaimKind::Pharmacy,
        "DENTAL" => ClaimKind::Oral,
        _ => ClaimKind::Medical,
    }
}

fn summary_amounts(b: &SummaryBalance) -> Amounts {
    let c = |m: &Option<Money>| Money::cents(m).unwrap_or_default();
    let billed = c(&b.total_billed_amount);
    let saved = c(&b.health_plan_discount);
    Amounts {
        billed,
        saved,
        allowed: Cents(billed.0 - saved.0),
        plan_paid: c(&b.health_plan_pays),
        deductible: c(&b.deductible),
        copay: c(&b.copay),
        // The summary's `other` is coinsurance and the not-covered amount
        // together; the claim's detail splits them.
        coinsurance: c(&b.other),
        not_covered: Cents(0),
        you_owe: c(&b.patient_responsibility),
    }
}

/// A pharmacy claim's one line, from the summary: the site gives no
/// detail of a prescription beyond its name, which it masks for some.
fn pharmacy_line(raw: &RawClaim, amounts: Amounts) -> ClaimLine {
    let name = text(&raw.medication_name).filter(|n| !n.chars().all(|c| c == '*'));
    ClaimLine {
        description: name.map_or_else(
            || "Prescription drug".to_string(),
            |n| n.split_whitespace().collect::<Vec<_>>().join(" "),
        ),
        date: date(&raw.service_date),
        amounts,
        ..ClaimLine::default()
    }
}

/// The claims answer read: members, then one record per claim id, newest
/// service first as the statements' are.
fn household(data: Value) -> Result<Household, CliError> {
    let data: ClaimsData = decode(data, "claims")?;
    let agg = data
        .all_claims_aggregate
        .ok_or_else(|| CliError::Upstream("the member site's claims answer holds no claims aggregate".into()))?;
    let members = agg.profile_v1.as_ref().map(members).unwrap_or_default();
    let raws: Vec<RawClaim> = agg.healthcare_claims.and_then(|h| h.claims).unwrap_or_default();
    let subscriber = members.first().map(|m| m.member_id.clone());
    let mut groups: BTreeMap<String, Vec<RawClaim>> = BTreeMap::new();
    let mut numbers: HashMap<String, String> = HashMap::new();
    for raw in raws {
        let (Some(claim_id), Some(kind)) = (text(&raw.claim_id), text(&raw.claim_type)) else {
            continue;
        };
        let (id, number) = claim_identity(&kind, &claim_id, raw.claim_id_suffix.as_deref());
        numbers.insert(id.clone(), number);
        groups.entry(id).or_default().push(raw);
    }
    let mut house = Household {
        members,
        ..Household::default()
    };
    for (id, mut versions) in groups {
        // Oldest processed first; one not processed yet is the newest.
        versions.sort_by(|a, b| {
            let key = |r: &RawClaim| (date(&r.processed_date).is_none(), date(&r.processed_date));
            key(a).cmp(&key(b))
        });
        let latest = versions.last().expect("a group has a version").clone();
        let kind_text = text(&latest.claim_type).unwrap_or_default();
        let kind = kind_of(&kind_text);
        let amounts_of = |r: &RawClaim| r.balance.as_ref().map(summary_amounts).unwrap_or_default();
        // A reversal: a later version with negative amounts. The claim keeps
        // the original's amounts.
        let negative = |a: &Amounts| a.billed.0 < 0 || a.plan_paid.0 < 0 || a.you_owe.0 < 0;
        let reversed = versions.iter().any(|v| negative(&amounts_of(v)));
        let base = if reversed {
            versions
                .iter()
                .rev()
                .find(|v| !negative(&amounts_of(v)))
                .unwrap_or(&latest)
        } else {
            &latest
        };
        let totals = amounts_of(base);
        let recipient = latest.service_recipient.clone().unwrap_or_default();
        let first = text(&recipient.first_name).unwrap_or_default();
        let who = by_seq(&house.members, recipient.dependent_seq_nbr.as_deref())
            .or_else(|| by_first_name(&house.members, &first));
        let (patient, label, member_id) = match who {
            Some(m) => (m.id.clone(), m.label.clone(), m.member_id.clone()),
            None => {
                let mid = subscriber.clone().unwrap_or_default();
                (patient_id(&mid, &first), title(&first), mid)
            }
        };
        let lines = if kind == ClaimKind::Pharmacy {
            vec![pharmacy_line(&latest, totals)]
        } else {
            Vec::new()
        };
        let keys: Vec<String> = versions.iter().filter_map(|v| text(&v.claim_key)).collect();
        if let Some(key) = text(&latest.claim_key) {
            house.keys.insert(
                id.clone(),
                DetailKey {
                    claim_key: key,
                    claim_type: kind_text.clone(),
                },
            );
        }
        house.all_keys.insert(id.clone(), keys);
        let service_start = date(&latest.service_date);
        let service_end = date(&latest.last_service_date).or_else(|| service_start.clone());
        house.claims.push(ClaimRecord {
            claim_number: numbers.get(&id).cloned(),
            id,
            patient,
            label,
            member_id,
            claim: Claim {
                patient: first,
                provider: text(&latest.provider_name),
                account_number: None,
                network: network(&latest.claim_network_status),
                claim_number: None,
                lines,
                total: Some(totals),
            },
            totals,
            service_start,
            service_end,
            statement_date: None,
            document: None,
            versions: versions.len() as u64,
            documents: Vec::new(),
            codes: BTreeMap::new(),
            kind,
            processed: date(&latest.processed_date),
            status: text(&latest.claim_status).map(|s| s.to_ascii_lowercase()),
            reversed,
            detailed: kind == ClaimKind::Pharmacy,
            origin: Origin::Site,
        });
    }
    for c in &house.claims {
        if let Some(m) = house.members.iter_mut().find(|m| m.id == c.patient) {
            m.claims += 1;
        }
    }
    house
        .claims
        .sort_by(|a, b| (b.service_start.as_deref(), &b.id).cmp(&(a.service_start.as_deref(), &a.id)));
    Ok(house)
}

/// A claim's detail read into it: its lines with their procedure, revenue
/// and remark codes, the detail's totals (coinsurance and the not-covered
/// amount apart), and whether it is institutional (a revenue code) or
/// professional.
fn apply_detail(c: &mut ClaimRecord, data: Value) -> Result<(), CliError> {
    let data: DetailData = decode(data, "claim detail")?;
    let Some(d) = data.claim_details_aggregate.and_then(|a| a.health_care_claim_details) else {
        return Err(CliError::Upstream(format!(
            "the member site answered no detail for claim {}",
            c.id
        )));
    };
    let mut lines = Vec::new();
    let mut codes = BTreeMap::new();
    for l in list(&d.service_lines) {
        let a = l.service_adjudication.as_ref();
        let m = |f: fn(&Adjudication) -> &Option<Money>| a.and_then(|a| Money::cents(f(a))).unwrap_or_default();
        let billed = m(|a| &a.charged_amount);
        let saved = m(|a| &a.provider_write_off);
        let allowed = a
            .and_then(|a| Money::cents(&a.allowed_amount))
            .unwrap_or(Cents(billed.0 - saved.0));
        let amounts = Amounts {
            billed,
            saved,
            allowed,
            plan_paid: m(|a| &a.plan_paid_amount),
            deductible: m(|a| &a.deductible),
            copay: m(|a| &a.copay),
            coinsurance: m(|a| &a.coinsurance),
            not_covered: m(|a| &a.member_not_covered_amount),
            you_owe: m(|a| &a.patient_responsibility),
        };
        let procedure = list(&l.procedures).iter().find_map(RawCode::coded).or_else(|| {
            l.service_info
                .as_ref()
                .and_then(RawCode::coded)
                .filter(|c| crate::fhir::procedure_system(crate::fhir::split_procedure(&c.code).0).is_some())
        });
        let (procedure, procedure_suffix) = match procedure {
            Some(p) => {
                let (code, suffix) = crate::fhir::split_procedure(&p.code);
                let suffix = suffix.map(str::to_string);
                (
                    Some(Coded {
                        code: code.to_string(),
                        display: p.display,
                    }),
                    suffix,
                )
            }
            None => (None, None),
        };
        let revenue = list(&l.revenues).iter().find_map(RawCode::coded);
        let description = [&l.service_info, &l.provided_service_info]
            .iter()
            .find_map(|i| i.as_ref().and_then(|i| text(&i.description)))
            .or_else(|| procedure.as_ref().and_then(|p| p.display.clone()))
            .or_else(|| revenue.as_ref().and_then(|r| r.display.clone()))
            .unwrap_or_else(|| "Service".to_string());
        let mut line_codes = Vec::new();
        for r in list(&l.service_remarks) {
            if let Some(code) = r.coded() {
                if let Some(text) = &code.display {
                    codes.entry(code.code.clone()).or_insert_with(|| text.clone());
                }
                if !line_codes.contains(&code.code) {
                    line_codes.push(code.code);
                }
            }
        }
        let start = date(&l.service_date);
        let end = date(&l.last_service_date).filter(|e| Some(e) != start.as_ref());
        lines.push(ClaimLine {
            description,
            codes: line_codes,
            date: start,
            date_end: end,
            amounts,
            procedure,
            procedure_suffix,
            revenue,
            tooth: text(&l.tooth_number),
        });
    }
    if let Some(b) = &d.balance {
        let c_ = |m: &Option<Money>| Money::cents(m).unwrap_or_default();
        let billed = c_(&b.total_billed_amount);
        let saved = c_(&b.health_plan_discount);
        c.totals = Amounts {
            billed,
            saved,
            allowed: Cents(billed.0 - saved.0),
            plan_paid: c_(&b.health_plan_pay),
            deductible: c_(&b.deductible),
            copay: c_(&b.copay),
            coinsurance: c_(&b.claim_coinsurance),
            not_covered: c_(&b.claim_member_not_covered),
            you_owe: c_(&b.patient_responsibility),
        };
        c.claim.total = Some(c.totals);
    }
    if c.kind == ClaimKind::Medical {
        c.kind = if lines.iter().any(|l| l.revenue.is_some()) {
            ClaimKind::Institutional
        } else {
            ClaimKind::Professional
        };
    }
    if let Some(n) = network(&d.claim_network_status) {
        c.claim.network = Some(n);
    }
    if let Some(p) = text(&d.provider_name) {
        c.claim.provider = Some(p);
    }
    if let Some(dates) = &d.claim_dates {
        c.processed = date(&dates.date_processed).or(c.processed.take());
        if let Some(s) = date(&dates.service_start_date) {
            c.service_start = Some(s);
        }
        if let Some(e) = date(&dates.last_service_date) {
            c.service_end = Some(e);
        }
    }
    c.claim.lines = lines;
    c.codes = codes;
    c.detailed = true;
    Ok(())
}

// ------------------------------------------------------------ balances

/// The plan balances: each member's medical deductible and out-of-pocket
/// progress against the plan's maximum, in and out of network, and the
/// family's once. `as_of` is today: the site shows the balances as they
/// stand. The plan year is the medical plan period's.
fn balances(data: Value, today: &str) -> Result<(Vec<Member>, Vec<Balances>), CliError> {
    let data: SpendingData = decode(data, "plan balances")?;
    let s = data
        .spending_overviews
        .ok_or_else(|| CliError::Upstream("the member site's plan balances answer is empty".into()))?;
    let members = s.profile_v1.as_ref().map(members).unwrap_or_default();
    // (dep seq, network, type) to the maximum.
    let mut maxes: HashMap<(String, Network, String), Cents> = HashMap::new();
    for dep in list(&s.benefits) {
        let seq = text(&dep.dep_seq_nbr).unwrap_or_default();
        for r in list(&dep.benefits_responses) {
            for b in list(&r.benefits).iter().filter(|b| is_medical(b)) {
                for (net, rows) in by_network(&b.maxes) {
                    for row in rows {
                        if let (Some(kind), Some(amount)) = (text(&row.kind), Money::cents(&row.amount)) {
                            maxes.insert((seq.clone(), net, kind), amount);
                        }
                    }
                }
            }
        }
    }
    let mut out: Vec<Balances> = Vec::new();
    for dep in list(&s.accumulators) {
        let seq = text(&dep.dep_seq_nbr).unwrap_or_default();
        let Some(m) = by_seq(&members, Some(&seq)) else {
            continue;
        };
        let plan_year = m
            .site
            .as_ref()
            .and_then(|s| s.plan_start.as_deref())
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse().ok());
        for summary in list(&dep.accumulator_summaries) {
            for b in list(&summary.benefits).iter().filter(|b| is_medical(b)) {
                for (net, rows) in by_network(&b.accumulators) {
                    for row in rows {
                        let Some(kind) = text(&row.kind) else { continue };
                        let Some((family, limit)) = limit_of(&kind) else {
                            continue;
                        };
                        let Some(used) = Money::cents(&row.amount) else {
                            continue;
                        };
                        let Some(maximum) = maxes.get(&(seq.clone(), net, kind.clone())).copied() else {
                            continue;
                        };
                        let (patient, label) = if family {
                            (family_id(&m.member_id), "Family".to_string())
                        } else {
                            (m.id.clone(), m.label.clone())
                        };
                        if out
                            .iter()
                            .any(|o| o.patient == patient && o.network == net && o.limit == limit)
                        {
                            continue;
                        }
                        out.push(Balances {
                            patient,
                            label,
                            member_id: m.member_id.clone(),
                            family,
                            plan_year,
                            as_of: Some(today.to_string()),
                            network: net,
                            limit,
                            maximum,
                            used,
                            remaining: Cents((maximum.0 - used.0).max(0)),
                            met: used >= maximum,
                            document: None,
                        });
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| {
        (b.plan_year, a.family, &a.label, a.network, a.limit).cmp(&(
            a.plan_year,
            b.family,
            &b.label,
            b.network,
            b.limit,
        ))
    });
    Ok((members, out))
}

fn is_medical(b: &Benefit) -> bool {
    b.coverage_type
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case("MEDICAL"))
}

fn by_network(n: &Option<ByNetwork>) -> [(Network, &[Accumulator]); 2] {
    match n {
        Some(n) => [
            (Network::In, list(&n.in_network)),
            (Network::Out, list(&n.out_of_network)),
        ],
        None => [(Network::In, &[]), (Network::Out, &[])],
    }
}

/// `INDIVIDUAL_OOP` to (not family, out-of-pocket). Dental's annual and
/// lifetime maximums are not deductible or out-of-pocket limits.
fn limit_of(kind: &str) -> Option<(bool, Limit)> {
    match kind.to_ascii_uppercase().as_str() {
        "INDIVIDUAL_DEDUCTIBLE" => Some((false, Limit::Deductible)),
        "INDIVIDUAL_OOP" => Some((false, Limit::OutOfPocket)),
        "FAMILY_DEDUCTIBLE" => Some((true, Limit::Deductible)),
        "FAMILY_OOP" => Some((true, Limit::OutOfPocket)),
        _ => None,
    }
}

// ----------------------------------------------------------- documents

/// The documents answer: one per document id (the site lists one row per
/// claim it covers), addressed to the member its rows name, newest first.
/// A row naming no member is one of the plan's general forms ("Other
/// Forms": proof of coverage and the like, no date, a class that is a title
/// rather than a document class), not the member's record: left out.
fn documents(data: Value, house: &Household) -> Result<Vec<SiteDocument>, CliError> {
    let data: DocumentsData = decode(data, "documents")?;
    let rows = data
        .multi_doc_type_list_summary
        .and_then(|l| l.documents)
        .unwrap_or_default();
    let mut out: Vec<SiteDocument> = Vec::new();
    for row in rows {
        let Some(id) = text(&row.document_id) else { continue };
        if text(&row.member_name).is_none() {
            continue;
        }
        if let Some(doc) = out.iter_mut().find(|d| d.id == id) {
            if let Some(key) = text(&row.claim_key) {
                if !doc.claim_keys.contains(&key) {
                    doc.claim_keys.push(key);
                }
            }
            continue;
        }
        let first = text(&row.member_name).and_then(|n| n.split_whitespace().next().map(str::to_string));
        let (patient, label) = match first {
            Some(first) => match by_first_name(&house.members, &first) {
                Some(m) => (Some(m.id.clone()), Some(m.label.clone())),
                None => (
                    house.member_id().map(|mid| patient_id(mid, &first)),
                    Some(title(&first)),
                ),
            },
            None => (None, None),
        };
        out.push(SiteDocument {
            id,
            class: text(&row.document_class).unwrap_or_default(),
            doc_type: text(&row.document_type).unwrap_or_else(|| "Document".into()),
            name: text(&row.document_name),
            date: date(&row.date_created),
            patient,
            label,
            claim_keys: text(&row.claim_key).into_iter().collect(),
        });
    }
    out.sort_by(|a, b| (b.date.as_deref(), &b.id).cmp(&(a.date.as_deref(), &a.id)));
    Ok(out)
}

/// The claims' documents, newest last, and the members' counts of EOBs.
pub fn link_documents(house: &mut Household, docs: &[SiteDocument]) {
    for c in &mut house.claims {
        let keys = house.all_keys.get(&c.id).map(Vec::as_slice).unwrap_or(&[]);
        let mut related: Vec<&SiteDocument> = docs
            .iter()
            .filter(|d| d.claim_keys.iter().any(|k| keys.contains(k)))
            .collect();
        related.sort_by(|a, b| (a.date.as_deref(), &a.id).cmp(&(b.date.as_deref(), &b.id)));
        c.documents = related.iter().map(|d| d.id.clone()).collect();
        c.document = related.iter().rev().find(|d| d.is_eob()).map(|d| d.id.clone());
    }
    for m in &mut house.members {
        let eobs: Vec<&SiteDocument> = docs
            .iter()
            .filter(|d| d.is_eob() && d.patient.as_deref() == Some(m.id.as_str()))
            .collect();
        m.statements = eobs.len() as u64;
        m.latest_statement = eobs.iter().filter_map(|d| d.date.clone()).max();
    }
}

// ---------------------------------------------------------------- Site

/// A profile's reads of the member site.
pub struct Site<'a> {
    client: Client,
    profile: ProfileName,
    session: Session,
    store: &'a SessionStore,
    today: String,
}

impl<'a> Site<'a> {
    /// The profile's stored session (none is exit 3, naming the sign-in),
    /// read against `base`, `pace` apart.
    pub fn open(
        store: &'a SessionStore,
        profile: &ProfileName,
        base: &str,
        pace: Duration,
    ) -> Result<Site<'a>, CliError> {
        let session = store.load(profile)?;
        let client = Client::new(&session, profile, base, pace)?;
        Ok(Site {
            client,
            profile: profile.clone(),
            session,
            store,
            today: crate::app::now_rfc3339()[..10].to_string(),
        })
    }

    pub fn today(&self) -> &str {
        &self.today
    }

    /// Members and every claim, from [`FIRST_DAY`] to today.
    pub fn household(&self) -> Result<Household, CliError> {
        let data = self.client.query(
            &client::CLAIMS,
            json!({ "startDate": FIRST_DAY, "stopDate": self.today }),
        )?;
        household(data)
    }

    /// Read `claim`'s detail into it. A pharmacy claim has none to read.
    pub fn detail(&self, house: &Household, claim: &mut ClaimRecord) -> Result<(), CliError> {
        if claim.detailed {
            return Ok(());
        }
        let Some(key) = house.keys.get(&claim.id) else {
            return Err(CliError::Upstream(format!(
                "the member site listed claim {} without a key to read its detail",
                claim.id
            )));
        };
        let data = self.client.query(
            &client::CLAIM_DETAILS,
            json!({ "claimKey": key.claim_key, "claimType": key.claim_type }),
        )?;
        apply_detail(claim, data)
    }

    /// The plan balances, and the members the answer names.
    pub fn balances(&self) -> Result<(Vec<Member>, Vec<Balances>), CliError> {
        let data = self.client.query(&client::SPENDING, json!({}))?;
        balances(data, &self.today)
    }

    /// Every document from [`FIRST_DAY`] to today.
    pub fn documents(&self, house: &Household) -> Result<Vec<SiteDocument>, CliError> {
        let data = self.client.query(
            &client::DOCUMENTS,
            json!({ "processedStartDate": FIRST_DAY, "processedStopDate": self.today }),
        )?;
        documents(data, house)
    }

    /// A document's PDF, checked to be one.
    pub fn pdf(&self, doc: &SiteDocument) -> Result<Vec<u8>, CliError> {
        let bytes = self.client.pdf(&doc.id, &doc.class)?;
        pk_cli_documents::verify_pdf(&bytes)?;
        Ok(bytes)
    }

    /// Write the session back when the answers updated its cookies: once,
    /// after the command read what it needed. Whether it was written.
    /// `uhc api`'s checked request ([`crate::client::api_request`]).
    pub fn api(&self, request: &crate::client::ApiRequest) -> Result<Value, CliError> {
        self.client.api(request)
    }

    /// Write the cookies the answers changed back to the profile's session,
    /// only while the stored session is still the one this command read: a
    /// sign-in made meanwhile (a long export runs for minutes) is never
    /// overwritten. Whether it wrote.
    pub fn finish(&self) -> Result<bool, CliError> {
        let Some(header) = self.client.updated_cookie_header() else {
            return Ok(false);
        };
        match self.store.get(&self.profile)? {
            Some(stored) if stored.cookie_header.expose() == self.session.cookie_header.expose() => {
                self.store
                    .put(&self.profile, &self.session.with_cookie_header(header))?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/site/{name}", env!("CARGO_MANIFEST_DIR"));
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        doc["data"].clone()
    }

    fn house() -> Household {
        let mut h = household(fixture("claims.json")).unwrap();
        let docs = documents(fixture("documents.json"), &h).unwrap();
        link_documents(&mut h, &docs);
        h
    }

    fn claim<'a>(h: &'a Household, id: &str) -> &'a ClaimRecord {
        h.claims
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no claim {id}"))
    }

    #[test]
    fn site_amounts_are_strings_or_numbers() {
        assert_eq!(amount(&json!("123.45")), Some(Cents(12345)));
        assert_eq!(amount(&json!("-5")), Some(Cents(-500)));
        assert_eq!(amount(&json!("1,234.5")), Some(Cents(123450)));
        assert_eq!(amount(&json!(12.5)), Some(Cents(1250)));
        assert_eq!(amount(&json!(0)), Some(Cents(0)));
        for bad in [json!("1.234"), json!("x"), json!(""), json!(null), json!("1.2.3")] {
            assert_eq!(amount(&bad), None, "{bad}");
        }
    }

    /// The ids a statement gives the same claims (tests/fixtures/eob).
    #[test]
    fn claim_ids_are_the_statements() {
        assert_eq!(
            claim_identity("MEDICAL", "SYNC000000-06", Some("06")),
            ("cSYNC00000006".into(), "SYNC00000006".into())
        );
        assert_eq!(
            claim_identity("MEDICAL", "SYNC000000", Some("08")),
            ("cSYNC00000008".into(), "SYNC00000008".into())
        );
        assert_eq!(claim_identity("MEDICAL", "SYNC11111111", None).0, "cSYNC11111111");
        assert_eq!(
            claim_identity("RX", "000000000000000009", None).0,
            "rx00000000000000000"
        );
        assert_eq!(claim_identity("DENTAL", "000000000003", None).0, "c000000000003");
    }

    #[test]
    fn members_are_the_statements() {
        let h = house();
        let labels: Vec<&str> = h.members.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, ["Alex", "Jordan", "Sam"]);
        let sam = h.members.iter().find(|m| m.label == "Sam").unwrap();
        assert_eq!(sam.id, patient_id("SYNM000001", "SAM"));
        assert_eq!(sam.relationship.as_deref(), Some("spouse"));
        assert_eq!(sam.group_number.as_deref(), Some("SYNG0001"));
        assert_eq!(sam.group_name, None);
        let site = sam.site.as_ref().unwrap();
        assert_eq!(site.gender.as_deref(), Some("female"));
        assert_eq!(site.birth_date.as_deref(), Some("1982-01-01"));
        assert_eq!(site.plan_start.as_deref(), Some("2026-01-01"));
        assert_eq!(site.full_name(), "Sam Example");
        let alex = &h.members[0];
        assert_eq!(alex.relationship.as_deref(), Some("self"));
        assert_eq!(h.members[1].relationship.as_deref(), Some("child"));
        assert_eq!(
            alex.claims, 3,
            "the dental claim, the reversed pharmacy claim, the reprocessed one"
        );
        assert_eq!(
            sam.statements, 2,
            "the medical and the pharmacy EOB; a health statement is not one"
        );
    }

    #[test]
    fn versions_and_reversals_are_one_claim() {
        let h = house();
        assert_eq!(h.claims.len(), 9);
        let rx = claim(&h, "rx55500000000000000");
        assert!(rx.reversed);
        assert_eq!(rx.versions, 2);
        assert!(rx.totals.billed.0 > 0, "the original's amounts");
        assert_eq!(rx.kind, ClaimKind::Pharmacy);
        assert_eq!(rx.claim.lines[0].description, "EXAMPLEDRUG 10 MG");
        let masked = claim(&h, "rx00000000000000000");
        assert_eq!(masked.claim.lines[0].description, "Prescription drug");
        assert_eq!(masked.totals.copay, Cents(1250));
        let reprocessed = claim(&h, "cSYNC22222222");
        assert_eq!(reprocessed.versions, 2);
        assert_eq!(reprocessed.processed.as_deref(), Some("2026-10-15"));
        assert_eq!(h.keys["cSYNC22222222"].claim_key, "SYNK-010");
        let pending = claim(&h, "cSYNC11111111");
        assert_eq!(pending.status.as_deref(), Some("in_process"));
        assert_eq!(pending.processed, None);
        let dental = claim(&h, "c000000000003");
        assert_eq!(dental.kind, ClaimKind::Oral);
        assert_eq!(dental.claim.network, Some(Network::Out));
    }

    #[test]
    fn a_claims_documents_are_the_ones_listing_it() {
        let h = house();
        let office = claim(&h, "cSYNC00000006");
        assert_eq!(office.document.as_deref(), Some("00000000-0000-4000-8000-000000000001"));
        let rx = claim(&h, "rx00000000000000000");
        assert_eq!(rx.document.as_deref(), Some("00000000-0000-4000-8000-000000000003"));
        let pending = claim(&h, "cSYNC11111111");
        assert_eq!(pending.document, None);
    }

    /// Some lines' procedure codes come with one more letter or digit. The
    /// code is split from it, so it carries its system, and the character
    /// stays on the line: two lines of one code can differ only in it.
    #[test]
    fn a_procedure_code_with_one_more_character_is_split_on_the_line() {
        let h = house();
        let mut c = claim(&h, "cSYNC00000008").clone();
        apply_detail(&mut c, fixture("claim-detail-suffixed.json")).unwrap();
        let got: Vec<(&str, Option<&str>)> = c
            .claim
            .lines
            .iter()
            .map(|l| {
                (
                    l.procedure.as_ref().unwrap().code.as_str(),
                    l.procedure_suffix.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("71046", Some("7")),
                ("71046", Some("8")),
                ("80061", Some("8")),
                ("E0110", Some("N")),
                ("99213", None),
            ]
        );
        assert_eq!(c.claim.lines[0].description, "Imaging/radiology");
        assert_eq!(c.totals.billed, Cents(45500));

        let eob = crate::fhir::explanation_of_benefit(&c, "Coverage/x", &[]);
        let first = &eob["item"][0]["productOrService"];
        assert_eq!(
            first["coding"],
            json!([
                { "system": "http://www.ama-assn.org/go/cpt", "code": "71046", "display": "Chest x-ray, 2 views" },
                { "system": crate::fhir::SITE_PROCEDURE_SYSTEM, "code": "710467" },
            ])
        );
        assert_eq!(first["text"], "Imaging/radiology");
        let dme = &eob["item"][3]["productOrService"]["coding"];
        assert_eq!(
            dme[0]["system"],
            "https://www.cms.gov/Medicare/Coding/HCPCSReleaseCodeSets"
        );
        assert_eq!(dme[0]["code"], "E0110");
        assert_eq!(dme[1]["code"], "E0110N");
        let plain = &eob["item"][4]["productOrService"]["coding"];
        assert_eq!(plain.as_array().unwrap().len(), 1, "{plain}");

        let v = crate::app::claim_json(&c, true);
        assert_eq!(
            v["lines"][0]["procedure"],
            json!({ "code": "71046", "display": "Chest x-ray, 2 views", "suffix": "7" })
        );
        assert!(v["lines"][4]["procedure"].get("suffix").is_none());
    }

    /// The detail of the statement's radiology claim (2026-08-20): the same
    /// totals, and the codes a statement does not print.
    #[test]
    fn a_detail_adds_lines_and_codes() {
        let h = house();
        let mut c = claim(&h, "cSYNC00000008").clone();
        apply_detail(&mut c, fixture("claim-detail-medical.json")).unwrap();
        assert_eq!(c.kind, ClaimKind::Professional);
        assert_eq!(c.totals.billed, Cents(52000));
        assert_eq!(c.totals.saved, Cents(25000));
        assert_eq!(c.totals.allowed, Cents(27000));
        assert_eq!(c.totals.plan_paid, Cents(17000));
        assert_eq!(c.totals.you_owe, Cents(10000));
        assert_eq!(c.claim.lines.len(), 2);
        let first = &c.claim.lines[0];
        assert_eq!(first.procedure.as_ref().unwrap().code, "70450");
        assert_eq!(first.amounts.allowed, Cents(15000), "charged less the write-off");
        assert_eq!(first.codes, ["D1"]);
        assert_eq!(c.claim.lines[1].procedure.as_ref().unwrap().code, "G2211");
        assert!(c.codes.contains_key("D1") && c.codes.contains_key("UG"));
        assert!(c.detailed);

        let mut inst = claim(&h, "cSYNC00000009").clone();
        apply_detail(&mut inst, fixture("claim-detail-institutional.json")).unwrap();
        assert_eq!(inst.kind, ClaimKind::Institutional);
        assert_eq!(inst.claim.lines[0].revenue.as_ref().unwrap().code, "0450");

        let mut dental = claim(&h, "c000000000003").clone();
        apply_detail(&mut dental, fixture("claim-detail-dental.json")).unwrap();
        assert_eq!(dental.kind, ClaimKind::Oral);
        let line = &dental.claim.lines[0];
        assert_eq!(line.procedure.as_ref().unwrap().code, "D1110");
        assert_eq!(line.tooth.as_deref(), Some("01-32"));
        assert_eq!(
            line.amounts.allowed,
            Cents(12000),
            "the dental answer's own allowed amount"
        );
    }

    #[test]
    fn balances_are_each_members_and_the_familys_once() {
        let (members, rows) = balances(fixture("spending.json"), "2026-11-17").unwrap();
        assert_eq!(members.len(), 3);
        // Three members and the family, two networks, two limits.
        assert_eq!(rows.len(), 16);
        let alex = patient_id("SYNM000001", "ALEX");
        let row = |p: &str, n: Network, l: Limit| {
            rows.iter()
                .find(|r| r.patient == p && r.network == n && r.limit == l)
                .unwrap()
        };
        let d = row(&alex, Network::In, Limit::Deductible);
        assert_eq!(
            (d.maximum, d.used, d.remaining),
            (Cents(125000), Cents(5000), Cents(120000))
        );
        assert_eq!(d.plan_year, Some(2026));
        assert_eq!(d.as_of.as_deref(), Some("2026-11-17"));
        let fam = row(&family_id("SYNM000001"), Network::In, Limit::OutOfPocket);
        assert_eq!((fam.maximum, fam.used), (Cents(800000), Cents(29250)));
        assert!(fam.family && fam.label == "Family");
        let out = row(&alex, Network::Out, Limit::Deductible);
        assert_eq!(out.maximum, Cents(300000));
    }

    #[test]
    fn documents_are_one_per_id_and_member() {
        let h = house();
        let docs = documents(fixture("documents.json"), &h).unwrap();
        assert_eq!(docs.len(), 5, "the form addressed to no one is left out");
        let eob = docs.iter().find(|d| d.id.ends_with("000000000001")).unwrap();
        assert_eq!(eob.claim_keys.len(), 2);
        assert!(eob.is_eob());
        assert_eq!(eob.category(), "eob");
        assert_eq!(eob.label.as_deref(), Some("Sam"));
        let letter = docs.iter().find(|d| d.id.ends_with("000000000005")).unwrap();
        assert_eq!(letter.category(), "prior-authorization-letters");
        assert!(!docs.iter().any(|d| d.id.ends_with("000000000006")));
        assert!(docs.iter().all(|d| d.patient.is_some()));
    }
}
