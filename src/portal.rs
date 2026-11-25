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
    family_id, patient_id, pharmacy_claim_id, sanitize, title, Balances, BenefitPeriod, ClaimKind, ClaimRecord, Member,
    Origin, SiteDocument, SiteMember, ORGANIZATION, ORGANIZATION_NAME,
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
    benefits_period_indicator: Option<String>,
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

/// The most lines [`left_out_lines`] tries sets of: 2^16 sets at most.
const LEFT_OUT_CANDIDATES: usize = 16;

/// The lines a claim's totals leave out, by index. The site's billed total
/// and amount owed can leave out a line that stays in the answer with its
/// own amounts (and the discount still counts its write-off). They
/// are the one set of lines the plan paid nothing on whose charges and
/// amounts owed are exactly what the totals are short of all the lines'.
/// None when the lines' plan paid differs from the total, when no set fits,
/// or when more than one does.
fn left_out_lines(lines: &[ClaimLine], billed: Cents, plan_paid: Cents, you_owe: Cents) -> Vec<usize> {
    let sum = |f: fn(&Amounts) -> Cents| lines.iter().map(|l| f(&l.amounts).0).sum::<i64>();
    let short_billed = sum(|a| a.billed) - billed.0;
    let short_owed = sum(|a| a.you_owe) - you_owe.0;
    if short_billed <= 0 || sum(|a| a.plan_paid) != plan_paid.0 {
        return Vec::new();
    }
    let candidates: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].amounts.plan_paid.0 == 0 && lines[i].amounts.billed.0 > 0)
        .collect();
    if candidates.is_empty() || candidates.len() > LEFT_OUT_CANDIDATES {
        return Vec::new();
    }
    let pick = |mask: u32| -> Vec<usize> {
        (0..candidates.len())
            .filter(|k| mask & (1 << k) != 0)
            .map(|k| candidates[k])
            .collect()
    };
    let mut found = None;
    for mask in 1u32..(1 << candidates.len()) {
        let set = pick(mask);
        let charged: i64 = set.iter().map(|&i| lines[i].amounts.billed.0).sum();
        let owed: i64 = set.iter().map(|&i| lines[i].amounts.you_owe.0).sum();
        if charged == short_billed && owed == short_owed {
            if found.is_some() {
                return Vec::new();
            }
            found = Some(set);
        }
    }
    found.unwrap_or_default()
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
            left_out: false,
        });
    }
    if let Some(b) = &d.balance {
        let c_ = |m: &Option<Money>| Money::cents(m).unwrap_or_default();
        let billed = c_(&b.total_billed_amount);
        let plan_paid = c_(&b.health_plan_pay);
        let left_out = left_out_lines(&lines, billed, plan_paid, c_(&b.patient_responsibility));
        for &i in &left_out {
            lines[i].left_out = true;
        }
        // The discount still counts a left-out line's write-off.
        let discount = c_(&b.health_plan_discount);
        let written_off: i64 = left_out.iter().map(|&i| lines[i].amounts.saved.0).sum();
        let saved = if discount.0 >= written_off {
            Cents(discount.0 - written_off)
        } else {
            discount
        };
        c.totals = Amounts {
            billed,
            saved,
            allowed: Cents(billed.0 - saved.0),
            plan_paid,
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

/// The member site's plan balances answer.
#[derive(Debug, Clone)]
pub struct PlanBalances {
    /// The members it names.
    pub members: Vec<Member>,
    pub balances: Vec<Balances>,
    /// The members (patient ids) whose medical plan the site says counts
    /// by calendar year, the only kind it names. It gives no dates.
    pub calendar_year: Vec<String>,
}

/// The plan balances: each member's medical deductible and out-of-pocket
/// progress against the plan's maximum, in and out of network, and the
/// family's once. `as_of` is today: the site shows the balances as they
/// stand. Where the medical plan counts by calendar year, the balances'
/// period is today's year; otherwise their plan year is the medical plan
/// period's start, and they have no period.
fn balances(data: Value, today: &str) -> Result<PlanBalances, CliError> {
    let data: SpendingData = decode(data, "plan balances")?;
    let s = data
        .spending_overviews
        .ok_or_else(|| CliError::Upstream("the member site's plan balances answer is empty".into()))?;
    let members = s.profile_v1.as_ref().map(members).unwrap_or_default();
    // The dependents whose medical benefits count by calendar year: the
    // site marks both the maximums and the progress.
    let mut calendar: Vec<String> = Vec::new();
    let mut mark = |seq: &Option<String>, b: &Benefit| {
        let seq = text(seq).unwrap_or_default();
        if is_medical(b) && by_calendar_year(b) && !calendar.contains(&seq) {
            calendar.push(seq);
        }
    };
    for dep in list(&s.benefits) {
        for r in list(&dep.benefits_responses) {
            list(&r.benefits).iter().for_each(|b| mark(&dep.dep_seq_nbr, b));
        }
    }
    for dep in list(&s.accumulators) {
        for summary in list(&dep.accumulator_summaries) {
            list(&summary.benefits).iter().for_each(|b| mark(&dep.dep_seq_nbr, b));
        }
    }
    let calendar_year: Vec<String> = members
        .iter()
        .filter(|m| {
            let seq = m.site.as_ref().map(|s| s.dep_seq.as_str()).unwrap_or_default();
            calendar.iter().any(|c| c == seq)
        })
        .map(|m| m.id.clone())
        .collect();
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
        let period = calendar_year
            .contains(&m.id)
            .then(|| BenefitPeriod::calendar_year_of(today))
            .flatten();
        let plan_year = match &period {
            Some(p) => p.year(),
            None => m
                .site
                .as_ref()
                .and_then(|s| s.plan_start.as_deref())
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse().ok()),
        };
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
                            period: period.clone(),
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
    Ok(PlanBalances {
        members,
        balances: out,
        calendar_year,
    })
}

fn is_medical(b: &Benefit) -> bool {
    b.coverage_type
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case("MEDICAL"))
}

fn by_calendar_year(b: &Benefit) -> bool {
    b.benefits_period_indicator
        .as_deref()
        .is_some_and(|i| i.trim().eq_ignore_ascii_case("CALENDAR_YEAR"))
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

    /// Reads as of `today` (ISO) instead of the clock's day: a test's.
    pub fn as_of(mut self, today: &str) -> Self {
        self.today = today.to_string();
        self
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

    /// The plan balances, the members the answer names and which of them
    /// the plan counts by calendar year.
    pub fn balances(&self) -> Result<PlanBalances, CliError> {
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

    const MEMBER: &str = "SYNM000001";
    const TODAY: &str = "2026-11-20";

    /// The `data` of an answer in `tests/fixtures/site/`.
    macro_rules! answer {
        ($name:literal) => {
            serde_json::from_str::<Value>(include_str!(concat!("../tests/fixtures/site/", $name))).unwrap()["data"]
                .take()
        };
    }

    fn usd(s: &str) -> Cents {
        Cents::parse(s).unwrap()
    }

    /// Nine amounts, in a statement's column order.
    fn nine(v: [&str; 9]) -> Amounts {
        let [billed, saved, allowed, plan_paid, deductible, copay, coinsurance, not_covered, you_owe] = v.map(usd);
        Amounts {
            billed,
            saved,
            allowed,
            plan_paid,
            deductible,
            copay,
            coinsurance,
            not_covered,
            you_owe,
        }
    }

    fn house() -> Household {
        household(answer!("claims.json")).unwrap()
    }

    /// The household with its documents.
    fn linked() -> Household {
        let mut h = house();
        let docs = documents(answer!("documents.json"), &h).unwrap();
        link_documents(&mut h, &docs);
        h
    }

    fn claim(h: &Household, id: &str) -> ClaimRecord {
        let found = h.claims.iter().find(|c| c.id == id);
        found.unwrap_or_else(|| panic!("no claim {id}")).clone()
    }

    /// Claim `id` with `detail` read into it.
    fn detailed(id: &str, detail: Value) -> ClaimRecord {
        let mut c = claim(&house(), id);
        apply_detail(&mut c, detail).unwrap();
        c
    }

    fn fhir_of(c: &ClaimRecord) -> Value {
        crate::fhir::explanation_of_benefit(c, "cov-synthetic", &[], None)
    }

    /// The codes of a line's or a claim's adjudication entries.
    fn categories(entries: &Value) -> Vec<&str> {
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["category"]["coding"][0]["code"].as_str().unwrap())
            .collect()
    }

    fn amount_of(entries: &Value, code: &str) -> Value {
        let entries = entries.as_array().unwrap();
        let found = entries.iter().find(|e| e["category"]["coding"][0]["code"] == code);
        found.map(|e| e["amount"]["value"].clone()).unwrap_or_default()
    }

    /// The site's service lines, to change one.
    fn service_lines(detail: &mut Value) -> &mut Value {
        &mut detail["claimDetailsAggregate"]["healthCareClaimDetails"]["serviceLines"]
    }

    // ---------------------------------------------------------- identity

    #[test]
    fn a_claim_is_named_by_its_kind_its_id_and_its_suffix() {
        for (kind, claim_id, suffix, id, number) in [
            // The suffix in the id and beside it, beside it only, in the id
            // only, and none.
            ("MEDICAL", "SYNC555118-62", Some("62"), "cSYNC55511862", "SYNC55511862"),
            ("MEDICAL", "SYNC555127", Some("85"), "cSYNC55512785", "SYNC55512785"),
            ("MEDICAL", "SYNC555140-09", None, "cSYNC55514009", "SYNC55514009"),
            ("MEDICAL", "SYNC55531742", None, "cSYNC55531742", "SYNC55531742"),
            // The suffix field wins over the id's own, and a blank one is none.
            ("MEDICAL", "SYNC555140-09", Some("10"), "cSYNC55514010", "SYNC55514010"),
            (
                "MEDICAL",
                " SYNC555140-09 ",
                Some("  "),
                "cSYNC55514009",
                "SYNC55514009",
            ),
            // A pharmacy claim: the first 17 of its 18 digits, whatever the
            // type's case. A number of another length is not one.
            (
                "RX",
                "555000000000002374",
                None,
                "rx55500000000000237",
                "555000000000002374",
            ),
            (
                "rx",
                "55500000000000237",
                None,
                "rx55500000000000237",
                "55500000000000237",
            ),
            ("RX", "5550000000000023", None, "c5550000000000023", "5550000000000023"),
            // The same digits on a claim that is not a pharmacy's.
            (
                "MEDICAL",
                "555000000000002374",
                None,
                "c555000000000002374",
                "555000000000002374",
            ),
            ("DENTAL", "555040000318", None, "c555040000318", "555040000318"),
            // An id holds only what a FHIR id may; the number is as listed.
            ("MEDICAL", "SYNC/555127", None, "cSYNC555127", "SYNC/555127"),
        ] {
            assert_eq!(
                claim_identity(kind, claim_id, suffix),
                (id.to_string(), number.to_string()),
                "{kind} {claim_id} {suffix:?}"
            );
        }
    }

    /// A statement prints a pharmacy claim's number cut to 17 digits.
    #[test]
    fn a_pharmacy_claim_has_the_id_its_statement_gives_it() {
        let (id, _) = claim_identity("RX", "555000000000002374", None);
        assert_eq!(pharmacy_claim_id("55500000000000237"), Some(id));
    }

    // ----------------------------------------------------------- members

    #[test]
    fn members_are_read_from_the_profile() {
        let h = linked();
        let row = |m: &Member| {
            let site = m.site.clone().unwrap();
            (
                m.label.clone(),
                m.relationship_code.clone().unwrap(),
                m.relationship.clone().unwrap(),
                site.dep_seq.clone(),
                site.full_name(),
                site.birth_date.unwrap(),
                site.gender.unwrap(),
                site.plan_start.unwrap(),
            )
        };
        let expected = [
            (
                "Alex",
                "SUBSCRIBER",
                "self",
                "00",
                "Alex Example",
                "1984-01-01",
                "female",
                "2023-04-01",
            ),
            (
                "Jordan",
                "CHILD",
                "child",
                "02",
                "Jordan Example",
                "2015-01-01",
                "female",
                "2026-02-01",
            ),
            (
                "Sam",
                "SPOUSE",
                "spouse",
                "01",
                "Sam Example",
                "1986-01-01",
                "male",
                "2026-03-16",
            ),
        ]
        .map(|(a, b, c, d, e, f, g, h)| {
            let s = str::to_string;
            (s(a), s(b), s(c), s(d), s(e), s(f), s(g), s(h))
        });
        // The plan start is the medical coverage's, whichever is listed first.
        assert_eq!(h.members.iter().map(row).collect::<Vec<_>>(), expected);
        for m in &h.members {
            assert_eq!(m.id, patient_id(MEMBER, &m.label));
            assert_eq!(m.member_id, MEMBER);
            assert_eq!(m.group_number.as_deref(), Some("SYNG0001"));
            assert_eq!(m.group_name, None, "the site leaves a null field out");
            assert_eq!(m.origin, Origin::Site);
            let site = m.site.as_ref().unwrap();
            assert_eq!(site.plan_end.as_deref(), Some("9999-12-31"));
            assert_eq!(site.plan_status.as_deref(), Some("ACTIVE"));
        }
        // Claims once each whatever their versions; EOBs, not the health
        // statement or the letter.
        let counts: Vec<_> = h
            .members
            .iter()
            .map(|m| (m.claims, m.statements, m.latest_statement.as_deref()))
            .collect();
        assert_eq!(
            counts,
            [
                (4, 5, Some("2026-08-08")),
                (2, 2, Some("2026-10-09")),
                (6, 2, Some("2026-07-02")),
            ]
        );
    }

    #[test]
    fn a_member_becomes_a_patient_and_a_coverage() {
        let h = house();
        let sam = &h.members[2];
        let p = crate::fhir::patient(sam, Some(&sam.site.as_ref().unwrap().full_name()));
        assert_eq!(p["identifier"][0]["value"], MEMBER);
        assert_eq!(
            p["name"],
            json!([{"text": "Sam Example", "family": "Example", "given": ["Sam"]}])
        );
        assert_eq!((&p["birthDate"], &p["gender"]), (&json!("1986-01-01"), &json!("male")));
        let mut sam = sam.clone();
        let c = crate::fhir::coverage(&sam);
        assert_eq!(c["status"], "active");
        assert_eq!(c["subscriberId"], MEMBER);
        assert_eq!(c["relationship"]["coding"][0]["code"], "spouse");
        assert_eq!(c["period"], json!({"start": "2026-03-16", "end": "9999-12-31"}));
        assert_eq!(c["class"][0]["value"], "SYNG0001");
        assert!(c["class"][0].get("name").is_none());
        sam.site.as_mut().unwrap().plan_status = Some("ENDED".into());
        assert_eq!(crate::fhir::coverage(&sam)["status"], "cancelled");
    }

    /// A claim or document naming someone the profile does not list is
    /// filed under the subscriber's member id and that first name.
    #[test]
    fn a_member_the_profile_does_not_list_is_named_from_the_answer() {
        let mut data = answer!("claims.json");
        data["allClaimsAggregate"]["profileV1"]["dependents"] = Value::Null;
        let h = household(data).unwrap();
        assert_eq!(h.members.len(), 1);
        let c = claim(&h, "cSYNC55520613");
        assert_eq!((c.patient, c.label), (patient_id(MEMBER, "SAM"), "Sam".to_string()));
        let docs = documents(answer!("documents.json"), &h).unwrap();
        let doc = docs.iter().find(|d| d.id.ends_with("a5")).unwrap();
        assert_eq!(doc.patient, Some(patient_id(MEMBER, "SAM")));
        assert_eq!(doc.label.as_deref(), Some("Sam"));
    }

    // ------------------------------------------------------------ claims

    #[test]
    fn each_claim_is_one_record_newest_service_first() {
        let h = house();
        let ids: Vec<&str> = h.claims.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "cSYNC555248",
                "c555040000318",
                "cSYNC55531742",
                "cSYNC55523128",
                "cSYNC55512785",
                "cSYNC55521940",
                "cSYNC55514009",
                "cSYNC55526407",
                "cSYNC55520613",
                "rx55500000000000237",
                "rx55500000000000481",
                "cSYNC55511862",
            ]
        );
        for c in &h.claims {
            assert_eq!(c.origin, Origin::Site);
            assert_eq!(c.member_id, MEMBER);
            assert!(h.keys.contains_key(&c.id), "{}", c.id);
            // Only a pharmacy claim is whole without its detail.
            assert_eq!(c.detailed, c.kind == ClaimKind::Pharmacy, "{}", c.id);
        }
    }

    #[test]
    fn a_reprocessed_claim_is_its_latest_version() {
        let h = house();
        let c = claim(&h, "cSYNC55511862");
        assert_eq!(c.versions, 2);
        assert_eq!(c.processed.as_deref(), Some("2026-03-19"));
        assert_eq!(c.claim.network, Some(Network::In), "the first version's was out");
        assert!(!c.reversed);
        assert_eq!(
            c.totals,
            nine(["1162.00", "693.70", "468.30", "374.64", "0.00", "0.00", "93.66", "0.00", "93.66"])
        );
        // The detail is read under the latest version's key; the documents
        // may list the claim under either, whatever order the site's is.
        assert_eq!(
            h.keys[&c.id],
            DetailKey {
                claim_key: "SYNK-018".into(),
                claim_type: "MEDICAL".into()
            }
        );
        assert_eq!(h.all_keys[&c.id], ["SYNK-071", "SYNK-018"]);

        // A version not processed yet is the newest.
        let mut data = answer!("claims.json");
        let rows = data["allClaimsAggregate"]["healthcareClaims"]["claims"]
            .as_array_mut()
            .unwrap();
        let first = rows.iter_mut().find(|r| r["claimKey"] == "SYNK-071").unwrap();
        first["processedDate"] = Value::Null;
        first["claimStatus"] = "IN_PROCESS".into();
        let c = claim(&household(data).unwrap(), "cSYNC55511862");
        assert_eq!((c.versions, c.status.as_deref()), (2, Some("in_process")));
        assert_eq!(c.totals.deductible, usd("1162.00"));
    }

    #[test]
    fn a_reversed_pharmacy_claim_keeps_the_originals_amounts() {
        let h = house();
        let c = claim(&h, "rx55500000000000481");
        assert!(c.reversed);
        assert_eq!(c.versions, 2);
        assert_eq!(c.processed.as_deref(), Some("2026-04-09"), "the reversal's date");
        let original = nine([
            "318.60", "97.15", "221.45", "176.45", "0.00", "45.00", "0.00", "0.00", "45.00",
        ]);
        assert_eq!(c.totals, original);
        assert_eq!(c.claim.lines[0].amounts, original);
        assert_eq!(fhir_of(&c)["status"], "cancelled");
        let others = h.claims.iter().filter(|o| o.id != c.id);
        for o in others {
            assert!(!o.reversed, "{}", o.id);
            assert_eq!(fhir_of(o)["status"], "active", "{}", o.id);
        }
    }

    #[test]
    fn a_pharmacy_claim_is_one_line_named_where_the_site_names_the_drug() {
        let h = house();
        let named = claim(&h, "rx55500000000000237");
        assert_eq!(named.kind, ClaimKind::Pharmacy);
        assert_eq!(named.claim_number.as_deref(), Some("555000000000002374"));
        assert_eq!(named.claim.lines.len(), 1);
        assert_eq!(named.claim.lines[0].description, "FLUTICASONE SPR 50MCG");
        assert_eq!(named.claim.lines[0].date.as_deref(), Some("2026-04-03"));
        assert_eq!(named.claim.lines[0].amounts, named.totals);
        let masked = claim(&h, "rx55500000000000481");
        assert_eq!(masked.claim.lines[0].description, "Prescription drug");
        let item = &fhir_of(&masked)["item"][0];
        assert_eq!(item["productOrService"], json!({"text": "Prescription drug"}));
        assert_eq!(fhir_of(&masked)["type"]["coding"][0]["code"], "pharmacy");
    }

    #[test]
    fn a_claim_in_process_has_what_little_the_site_gives() {
        let c = claim(&house(), "cSYNC555248");
        assert_eq!(c.claim_number.as_deref(), Some("SYNC555248"));
        assert_eq!(c.status.as_deref(), Some("in_process"));
        assert_eq!((c.processed, c.claim.network), (None, None));
        assert_eq!(c.service_end.as_deref(), Some("2026-11-03"));
        // With no last day of service, the first is the last.
        let mut data = answer!("claims.json");
        let rows = data["allClaimsAggregate"]["healthcareClaims"]["claims"]
            .as_array_mut()
            .unwrap();
        rows.iter_mut().for_each(|r| r["lastServiceDate"] = Value::Null);
        let hospital = claim(&household(data).unwrap(), "cSYNC55512785");
        assert_eq!(hospital.service_end.as_deref(), Some("2026-07-18"));
        assert_eq!(
            c.totals,
            nine(["475.00", "0.00", "475.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00"])
        );
    }

    #[test]
    fn a_denied_claim_owes_nothing() {
        let before = claim(&house(), "cSYNC55523128");
        assert_eq!(before.status.as_deref(), Some("provider_denied"));
        let c = detailed("cSYNC55523128", answer!("claim-detail-provider-denied.json"));
        assert_eq!(c.claim.network, None, "the site gives none");
        assert!(fhir_of(&c).get("adjudication").is_none());
        assert_eq!(c.totals.saved, usd("2200.00"));
        assert_eq!((c.totals.allowed, c.totals.you_owe), (Cents(0), Cents(0)));
        assert_eq!(
            c.codes["W5"],
            "THE PROVIDER DID NOT SEND THE RECORDS WE ASKED FOR. YOU DO NOT OWE THIS AMOUNT."
        );
    }

    // --------------------------------------------------------- documents

    const DOC: &str = "00000000-0000-4000-8000-0000000000";

    fn doc_id(tail: &str) -> String {
        format!("{DOC}{tail}")
    }

    #[test]
    fn documents_are_one_per_id_and_member_newest_first() {
        let h = house();
        let docs = documents(answer!("documents.json"), &h).unwrap();
        let rows: Vec<(String, &str, String, &str, usize)> = docs
            .iter()
            .map(|d| {
                (
                    d.id.strip_prefix(DOC).unwrap().to_string(),
                    d.date.as_deref().unwrap(),
                    d.category(),
                    d.label.as_deref().unwrap(),
                    d.claim_keys.len(),
                )
            })
            .collect();
        let expected = [
            ("a4", "2026-10-09", "eob", "Jordan", 1),
            ("d4", "2026-10-07", "eob", "Jordan", 1),
            ("b2", "2026-09-01", "health-statement", "Alex", 0),
            ("a3", "2026-08-08", "eob", "Alex", 1),
            ("b6", "2026-08-04", "prior-authorization-letters", "Sam", 0),
            ("a9", "2026-07-02", "eob", "Sam", 1),
            ("a6", "2026-05-22", "eob", "Alex", 1),
            ("a5", "2026-05-07", "eob", "Sam", 2),
            ("a8", "2026-04-15", "eob", "Alex", 1),
            ("a2", "2026-03-23", "eob", "Alex", 1),
            ("a1", "2026-01-12", "eob", "Alex", 1),
        ]
        .map(|(id, date, category, who, claims)| (id.to_string(), date, category.to_string(), who, claims));
        // The plan's general form, addressed to no one, is not among them.
        assert_eq!(rows, expected);
        for d in &docs {
            assert_eq!(d.patient, Some(patient_id(MEMBER, d.label.as_deref().unwrap())));
        }
        // One statement's rows are one document, whose rows need not be
        // next to each other.
        let shared = &docs[7];
        assert_eq!(shared.claim_keys, ["SYNK-013", "SYNK-074"]);
        assert_eq!(shared.class, "u_mber_eob");
        assert_eq!(shared.name.as_deref(), Some("Claim EOB"));
        assert_eq!(docs[1].class, "u_sb_dental_elec");
        assert_eq!(docs[2].class, "u_mthly_health_stmt");
        assert_eq!(docs[4].class, "u_prior_auth_doc");
    }

    #[test]
    fn a_claims_documents_are_those_listing_any_of_its_versions() {
        let h = linked();
        let links = |id: &str| {
            let c = claim(&h, id);
            (c.documents, c.document)
        };
        assert_eq!(
            links("cSYNC55511862"),
            (vec![doc_id("a1"), doc_id("a2")], Some(doc_id("a2"))),
            "one EOB for each version, the later last"
        );
        // One EOB of two claims, then one each of a pharmacy's and a
        // dentist's.
        for (id, tail) in [
            ("cSYNC55526407", "a5"),
            ("cSYNC55520613", "a5"),
            ("rx55500000000000237", "a8"),
            ("c555040000318", "d4"),
        ] {
            assert_eq!(links(id), (vec![doc_id(tail)], Some(doc_id(tail))), "{id}");
        }
        for id in ["cSYNC55523128", "rx55500000000000481", "cSYNC555248"] {
            assert_eq!(links(id), (vec![], None), "{id}");
        }
    }

    #[test]
    fn a_claim_is_explained_in_its_latest_eob_not_a_later_letter() {
        let mut h = house();
        let mut data = answer!("documents.json");
        let rows = data["multiDocTypeListSummary"]["documents"].as_array_mut().unwrap();
        let letter = rows
            .iter_mut()
            .find(|r| r["documentType"] == "Prior Authorization Letters")
            .unwrap();
        letter["claimKey"] = "SYNK-056".into();
        let docs = documents(data, &h).unwrap();
        link_documents(&mut h, &docs);
        let c = claim(&h, "cSYNC55521940");
        assert_eq!(c.documents, [doc_id("a9"), doc_id("b6")]);
        assert_eq!(c.document, Some(doc_id("a9")));
    }

    // ------------------------------------------------------------ detail

    #[test]
    fn a_professional_detail_reads_its_lines_codes_and_totals() {
        let c = detailed("cSYNC55520613", answer!("claim-detail-professional.json"));
        assert_eq!(c.kind, ClaimKind::Professional);
        assert!(c.detailed);
        assert_eq!(c.claim.network, Some(Network::In));
        assert_eq!(c.processed.as_deref(), Some("2026-05-04"));
        assert_eq!(
            (c.service_start.as_deref(), c.service_end.as_deref()),
            (Some("2026-04-07"), Some("2026-04-21"))
        );
        let total = nine([
            "547.00", "337.90", "209.10", "65.20", "127.60", "0.00", "16.30", "0.00", "143.90",
        ]);
        assert_eq!((c.totals, c.claim.total), (total, Some(total)));
        let lines: Vec<_> = c
            .claim
            .lines
            .iter()
            .map(|l| {
                (
                    l.description.as_str(),
                    l.date.as_deref().unwrap(),
                    l.procedure.as_ref().unwrap().code.as_str(),
                    l.codes.join(" "),
                )
            })
            .collect();
        assert_eq!(
            lines,
            [
                ("THERAPY EVALUATION", "2026-04-07", "97161", "K4".to_string()),
                ("THERAPY EXERCISE", "2026-04-14", "97110", "K4".to_string()),
                ("HANDS-ON THERAPY", "2026-04-21", "97140", "K4".to_string()),
                ("ELECTRICAL STIMULATION", "2026-04-21", "G0283", String::new()),
            ]
        );
        // The site gives a medical line no allowed amount: it is what is
        // charged less the write-off.
        assert_eq!(
            c.claim.lines[1].amounts,
            nine(["148.00", "91.60", "56.40", "17.64", "34.35", "0.00", "4.41", "0.00", "38.76"])
        );
        for l in &c.claim.lines {
            assert_eq!((l.date_end.as_ref(), l.revenue.as_ref()), (None, None));
            assert_eq!(l.amounts.allowed.0, l.amounts.billed.0 - l.amounts.saved.0);
            assert!(!l.left_out);
        }
        assert_eq!(c.codes.keys().collect::<Vec<_>>(), ["K4"]);
    }

    #[test]
    fn an_institutional_detail_reads_revenue_codes_and_a_line_of_two_days() {
        let c = detailed("cSYNC55512785", answer!("claim-detail-institutional.json"));
        assert_eq!(c.kind, ClaimKind::Institutional);
        assert_eq!(
            c.totals,
            nine(["1893.00", "1086.97", "806.03", "329.38", "119.30", "275.00", "82.35", "0.00", "476.65"])
        );
        let revenue: Vec<_> = c
            .claim
            .lines
            .iter()
            .map(|l| l.revenue.as_ref().unwrap().code.as_str())
            .collect();
        assert_eq!(revenue, ["0450", "0324", "0305", "0636", "0258"]);
        let days: Vec<_> = c
            .claim
            .lines
            .iter()
            .map(|l| (l.date.as_deref().unwrap(), l.date_end.as_deref()))
            .collect();
        assert_eq!(
            days,
            [
                ("2026-07-18", Some("2026-07-19")),
                ("2026-07-18", None),
                ("2026-07-18", None),
                ("2026-07-19", None),
                ("2026-07-19", None),
            ]
        );
        assert_eq!(c.claim.lines[4].codes, ["Q9", "K4"], "as the site orders them");
        assert_eq!(
            c.codes.keys().collect::<Vec<_>>(),
            ["K4", "Q9"],
            "each once, in code order"
        );
        let items = fhir_of(&c)["item"].clone();
        assert_eq!(
            items[0]["servicedPeriod"],
            json!({"start": "2026-07-18", "end": "2026-07-19"})
        );
        assert_eq!(items[3]["servicedDate"], "2026-07-19");
        assert_eq!(
            items[0]["revenue"]["coding"][0],
            json!({"system": "https://www.nubc.org/CodeSystem/RevenueCodes", "code": "0450", "display": "EMERGENCY ROOM"})
        );
        // Two notes, numbered in code order.
        assert_eq!(items[4]["noteNumber"], json!([2, 1]));
    }

    #[test]
    fn a_dental_detail_reads_teeth_and_the_sites_allowed_amount() {
        let before = claim(&house(), "c555040000318");
        // The summary holds what is not covered with coinsurance.
        assert_eq!(
            (before.totals.coinsurance, before.totals.not_covered),
            (usd("92.10"), Cents(0))
        );
        let c = detailed("c555040000318", answer!("claim-detail-dental.json"));
        assert_eq!(c.kind, ClaimKind::Oral);
        assert_eq!(c.claim.network, Some(Network::Out));
        assert_eq!(
            c.totals,
            nine(["303.00", "0.00", "303.00", "180.90", "30.00", "0.00", "13.60", "78.50", "122.10"])
        );
        let lines: Vec<_> = c
            .claim
            .lines
            .iter()
            .map(|l| {
                (
                    l.procedure.as_ref().unwrap().code.as_str(),
                    l.tooth.as_deref(),
                    l.amounts.billed,
                    l.amounts.allowed,
                )
            })
            .collect();
        assert_eq!(
            lines,
            [
                ("D0120", None, usd("71.00"), usd("52.00")),
                ("D1120", Some("01-32"), usd("96.00"), usd("74.50")),
                ("D1351", Some("19"), usd("68.00"), usd("49.00")),
                ("D1351", Some("30"), usd("68.00"), usd("49.00")),
            ]
        );
    }

    #[test]
    fn a_detail_splits_coinsurance_from_what_is_not_covered() {
        let id = "cSYNC55526407";
        let before = claim(&house(), id).totals;
        assert_eq!(
            (before.coinsurance, before.not_covered),
            (usd("178.00"), Cents(0)),
            "together in the summary"
        );
        let c = detailed(id, answer!("claim-detail-out-of-network.json"));
        assert_eq!(
            (c.totals.coinsurance, c.totals.not_covered),
            (usd("22.80"), usd("155.20"))
        );
        assert_eq!(
            Amounts {
                coinsurance: before.coinsurance,
                not_covered: Cents(0),
                ..c.totals
            },
            before
        );
    }

    #[test]
    fn a_detail_the_site_does_not_answer_is_an_error_naming_the_claim() {
        let mut c = claim(&house(), "cSYNC55520613");
        let e = apply_detail(&mut c, json!({"claimDetailsAggregate": null})).unwrap_err();
        assert_eq!(e.exit_code(), 5);
        assert!(e.to_string().contains("cSYNC55520613"), "{e}");
        assert!(!c.detailed);
    }

    // --------------------------------------------------------- procedures

    #[test]
    fn a_character_the_site_appends_to_a_code_is_split_from_it() {
        let hospital = detailed("cSYNC55512785", answer!("claim-detail-institutional.json"));
        let therapy = detailed("cSYNC55520613", answer!("claim-detail-professional.json"));
        for (c, n, code, suffix, system, display, text) in [
            (
                &hospital,
                1,
                "71046",
                "R",
                "http://www.ama-assn.org/go/cpt",
                "X-RAY OF THE CHEST, 2 VIEWS",
                "CHEST X-RAY",
            ),
            (
                &therapy,
                3,
                "G0283",
                "4",
                "https://www.cms.gov/Medicare/Coding/HCPCSReleaseCodeSets",
                "ELECTRICAL STIMULATION, UNATTENDED",
                "ELECTRICAL STIMULATION",
            ),
        ] {
            let site_code = format!("{code}{suffix}");
            let line = &c.claim.lines[n];
            let procedure = Coded {
                code: code.into(),
                display: Some(display.into()),
            };
            assert_eq!(line.procedure, Some(procedure));
            assert_eq!(line.procedure_suffix.as_deref(), Some(suffix));
            assert_eq!(
                fhir_of(c)["item"][n]["productOrService"],
                json!({
                    "coding": [
                        {"system": system, "code": code, "display": display},
                        {"system": crate::fhir::SITE_PROCEDURE_SYSTEM, "code": site_code},
                    ],
                    "text": text,
                })
            );
            let v = crate::app::claim_json(c, true);
            assert_eq!(
                v["lines"][n]["procedure"],
                json!({"code": code, "display": display, "suffix": suffix, "site_code": site_code})
            );
            // The other lines have one coding, and neither key.
            for (k, l) in c.claim.lines.iter().enumerate().filter(|(k, _)| *k != n) {
                assert_eq!(l.procedure_suffix, None);
                let p = &v["lines"][k]["procedure"];
                assert!(p.get("suffix").is_none() && p.get("site_code").is_none(), "{p}");
                let codings = fhir_of(c)["item"][k]["productOrService"]["coding"].clone();
                assert_eq!(codings.as_array().unwrap().len(), 1);
            }
        }
    }

    #[test]
    fn a_line_without_a_usable_procedure_takes_the_service_code_of_that_shape() {
        let with = |procedures: Value, service: &str| {
            let mut data = answer!("claim-detail-professional.json");
            let line = &mut service_lines(&mut data)[0];
            line["procedures"] = procedures;
            line["serviceInfo"]["code"] = service.into();
            detailed("cSYNC55520613", data)
        };
        let code = |c: &ClaimRecord| {
            let l = &c.claim.lines[0];
            (l.procedure.as_ref().map(|p| p.code.clone()), l.procedure_suffix.clone())
        };
        let some = |s: &str| Some(s.to_string());
        assert_eq!(code(&with(json!([]), "97161")), (some("97161"), None));
        let blank = json!([{"code": " ", "description": ""}]);
        assert_eq!(code(&with(blank, "71046R")), (some("71046"), some("R")));
        // A service code of no procedure's shape is not taken.
        let c = with(json!([]), "0420");
        assert_eq!(code(&c), (None, None));
        assert_eq!(
            fhir_of(&c)["item"][0]["productOrService"],
            json!({"text": "THERAPY EVALUATION"})
        );
        // A procedure code of another shape is kept as text.
        let c = with(json!([{"code": "LOCAL7", "description": "CLINIC FEE"}]), "97161");
        assert_eq!(code(&c), (some("LOCAL7"), None));
        assert_eq!(
            fhir_of(&c)["item"][0]["productOrService"],
            json!({"text": "LOCAL7 THERAPY EVALUATION"})
        );
    }

    // ---------------------------------------------------- left-out lines

    #[test]
    fn lines_the_totals_leave_out_are_marked_and_their_write_offs_taken_out() {
        let id = "cSYNC55521940";
        let before = claim(&house(), id).totals;
        assert_eq!(
            (before.billed, before.saved, before.allowed),
            (usd("329.00"), usd("397.85"), usd("-68.85")),
            "the summary's discount counts the lines its billed total leaves out"
        );
        let c = detailed(id, answer!("claim-detail-left-out-discount.json"));
        let marked: Vec<bool> = c.claim.lines.iter().map(|l| l.left_out).collect();
        assert_eq!(
            marked,
            [false, false, false, true, true],
            "not the vitamin D line, which the plan paid nothing on too"
        );
        assert_eq!(
            c.totals,
            nine(["329.00", "267.85", "61.15", "61.15", "0.00", "0.00", "0.00", "0.00", "0.00"])
        );
        assert_eq!(c.claim.total, Some(c.totals));
        // A marked line keeps the site's amounts.
        assert_eq!(
            c.claim.lines[3].amounts,
            nine(["92.00", "92.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00"])
        );

        let v = fhir_of(&c);
        for (n, item) in v["item"].as_array().unwrap().iter().enumerate() {
            let entries = &item["adjudication"];
            let last = entries.as_array().unwrap().last().unwrap();
            if n < 3 {
                assert!(!categories(entries).contains(&"not-counted"), "line {n}");
                continue;
            }
            assert_eq!(
                *last,
                json!({"category": {"coding": [{
                    "system": "https://github.com/own-your-health/uhc-cli/fhir/adjudication",
                    "code": "not-counted",
                    "display": "Not counted in the claim's totals",
                }]}}),
                "a mark, with no amount"
            );
            assert_eq!(entries.as_array().unwrap().len(), 10, "beside the nine amounts");
        }
        assert_eq!(amount_of(&v["total"], "discount"), 267.85);
        assert_eq!(amount_of(&v["total"], "eligible"), 61.15);
        let counted: Vec<Value> = crate::app::claim_json(&c, true)["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["counted"].clone())
            .collect();
        assert_eq!(counted, [true, true, true, false, false]);
    }

    #[test]
    fn a_left_out_line_can_have_an_amount_owed() {
        let c = detailed("cSYNC55514009", answer!("claim-detail-left-out-owed.json"));
        let marked: Vec<bool> = c.claim.lines.iter().map(|l| l.left_out).collect();
        assert_eq!(marked, [false, true, false, false]);
        assert_eq!(c.claim.lines[1].amounts.you_owe, usd("28.00"));
        assert_eq!(c.claim.lines[1].amounts.not_covered, usd("28.00"));
        assert_eq!(
            c.totals,
            nine(["535.50", "251.15", "284.35", "111.08", "118.40", "45.00", "9.87", "0.00", "173.27"]),
            "neither its charge nor what is owed on it"
        );
        let v = fhir_of(&c);
        assert!(categories(&v["item"][1]["adjudication"]).contains(&"not-counted"));
        assert_eq!(amount_of(&v["item"][1]["adjudication"], "memberliability"), 28.0);
        assert_eq!(amount_of(&v["total"], "memberliability"), 173.27);
    }

    #[test]
    fn a_claim_whose_totals_count_every_line_has_none_marked() {
        for (id, detail) in [
            ("cSYNC55511862", answer!("claim-detail-reprocessed.json")),
            ("cSYNC55512785", answer!("claim-detail-institutional.json")),
            ("cSYNC55520613", answer!("claim-detail-professional.json")),
            ("cSYNC55523128", answer!("claim-detail-provider-denied.json")),
            ("cSYNC55531742", answer!("claim-detail-no-suffix.json")),
            ("cSYNC55526407", answer!("claim-detail-out-of-network.json")),
            ("c555040000318", answer!("claim-detail-dental.json")),
        ] {
            let c = detailed(id, detail);
            assert!(c.claim.lines.iter().all(|l| !l.left_out), "{id}");
            let sum = c.claim.lines.iter().fold(Amounts::default(), |a, l| a + l.amounts);
            assert_eq!((sum.billed, sum.saved), (c.totals.billed, c.totals.saved), "{id}");
            assert_eq!(
                (sum.plan_paid, sum.you_owe),
                (c.totals.plan_paid, c.totals.you_owe),
                "{id}"
            );
        }
    }

    /// With no set marked, the site's discount is kept as given; an allowed
    /// amount it makes negative is not written.
    #[test]
    fn totals_no_set_of_lines_explains_are_kept_as_given() {
        let mut data = answer!("claim-detail-left-out-discount.json");
        data["claimDetailsAggregate"]["healthCareClaimDetails"]["balance"]["totalBilledAmount"]["value"] =
            "300.00".into();
        let c = detailed("cSYNC55521940", data);
        assert!(c.claim.lines.iter().all(|l| !l.left_out));
        assert_eq!((c.totals.saved, c.totals.allowed), (usd("397.85"), usd("-97.85")));
        let total = fhir_of(&c)["total"].clone();
        assert!(!categories(&total).contains(&"eligible"), "{total}");
        assert_eq!(amount_of(&total, "discount"), 397.85);
    }

    /// Lines as (charged, plan paid, owed) in cents; the rest written off.
    fn lines(rows: &[(i64, i64, i64)]) -> Vec<ClaimLine> {
        rows.iter()
            .map(|&(charged, paid, owed)| ClaimLine {
                amounts: Amounts {
                    billed: Cents(charged),
                    saved: Cents(charged - paid - owed),
                    allowed: Cents(paid + owed),
                    plan_paid: Cents(paid),
                    you_owe: Cents(owed),
                    ..Amounts::default()
                },
                ..ClaimLine::default()
            })
            .collect()
    }

    /// What [`left_out_lines`] marks when the totals are short of the lines'
    /// by `billed` and `owed`.
    fn short_by(lines: &[ClaimLine], billed: i64, owed: i64) -> Vec<usize> {
        let all = lines.iter().fold(Amounts::default(), |a, l| a + l.amounts);
        left_out_lines(
            lines,
            Cents(all.billed.0 - billed),
            all.plan_paid,
            Cents(all.you_owe.0 - owed),
        )
    }

    #[test]
    fn the_left_out_lines_are_the_one_set_that_fits() {
        let l = lines(&[(9_150, 6_020, 0), (4_310, 0, 0), (2_675, 0, 2_140), (7_390, 0, 5_505)]);
        assert_eq!(short_by(&l, 2_675, 2_140), [2]);
        assert_eq!(short_by(&l, 4_310, 0), [1]);
        assert_eq!(short_by(&l, 4_310 + 7_390, 5_505), [1, 3]);
        assert_eq!(short_by(&l, 4_310 + 2_675 + 7_390, 2_140 + 5_505), [1, 2, 3]);
        // A line charged nothing is in no set, so it makes no second one.
        let l = lines(&[(9_150, 6_020, 0), (2_675, 0, 2_140), (0, 0, 0)]);
        assert_eq!(short_by(&l, 2_675, 2_140), [1]);
    }

    #[test]
    fn no_line_is_marked_where_the_amounts_do_not_decide() {
        let l = lines(&[(9_150, 6_020, 0), (4_310, 0, 0), (2_675, 0, 2_140), (7_390, 0, 5_505)]);
        let none: [usize; 0] = [];
        // Nothing is short, or the totals are over.
        assert_eq!(short_by(&l, 0, 0), none);
        assert_eq!(short_by(&l, 0, 2_140), none);
        assert_eq!(short_by(&l, -2_675, -2_140), none);
        // No set fits: the charge alone, or what is owed alone.
        assert_eq!(short_by(&l, 2_675, 0), none);
        assert_eq!(short_by(&l, 2_676, 2_140), none);
        // A line the plan paid on is never one.
        assert_eq!(short_by(&l, 9_150, 0), none);
        // Two sets fit: two lines alike, or one line and two others.
        let alike = lines(&[(9_150, 6_020, 0), (2_675, 0, 2_140), (2_675, 0, 2_140)]);
        assert_eq!(short_by(&alike, 2_675, 2_140), none);
        assert_eq!(short_by(&alike, 5_350, 4_280), [1, 2], "both is one set");
        let sums = lines(&[(9_150, 6_020, 0), (5_000, 0, 0), (3_000, 0, 0), (2_000, 0, 0)]);
        assert_eq!(short_by(&sums, 5_000, 0), none);
        // The lines' plan paid is not the claim's.
        let all = l.iter().fold(Amounts::default(), |a, l| a + l.amounts);
        for paid in [all.plan_paid.0 - 1, all.plan_paid.0 + 1] {
            assert_eq!(
                left_out_lines(
                    &l,
                    Cents(all.billed.0 - 2_675),
                    Cents(paid),
                    Cents(all.you_owe.0 - 2_140)
                ),
                none
            );
        }
        // No lines at all.
        assert_eq!(left_out_lines(&[], Cents(-100), Cents(0), Cents(0)), none);
    }

    #[test]
    fn no_line_is_marked_beyond_the_most_sets_tried() {
        // Each line's charge is twice the one before's, so one set makes up
        // any sum. A line the plan paid on is no candidate.
        let of = |candidates: usize| {
            let mut rows: Vec<(i64, i64, i64)> = (0..candidates).map(|k| (1 << k, 0, 0)).collect();
            rows.push((500, 120, 0));
            lines(&rows)
        };
        assert_eq!(short_by(&of(LEFT_OUT_CANDIDATES), 5, 0), [0, 2]);
        assert!(short_by(&of(LEFT_OUT_CANDIDATES + 1), 5, 0).is_empty());
    }

    // ---------------------------------------------------------- balances

    /// Every benefit of a plan balances answer, with the list it is in.
    fn each_benefit(data: &mut Value, mut f: impl FnMut(&str, &mut Value)) {
        for (list, inner) in [
            ("benefits", "benefitsResponses"),
            ("accumulators", "accumulatorSummaries"),
        ] {
            for dep in data["spendingOverviews"][list].as_array_mut().unwrap() {
                for response in dep[inner].as_array_mut().unwrap() {
                    for benefit in response["benefits"].as_array_mut().unwrap() {
                        f(list, benefit);
                    }
                }
            }
        }
    }

    #[test]
    fn balances_are_each_members_and_the_familys_once() {
        let b = balances(answer!("spending.json"), TODAY).unwrap();
        let rows: Vec<_> = b
            .balances
            .iter()
            .map(|r| {
                (
                    r.label.as_str(),
                    r.network.code(),
                    r.limit.code(),
                    [r.maximum, r.used, r.remaining],
                    r.met,
                )
            })
            .collect();
        let (ded, oop) = ("deductible", "out_of_pocket");
        let expected = [
            ("Alex", "in", ded, ["1150.00", "1150.00", "0.00"], true),
            ("Alex", "in", oop, ["5200.00", "1893.44", "3306.56"], false),
            ("Alex", "out", ded, ["2450.00", "0.00", "2450.00"], false),
            ("Alex", "out", oop, ["9750.00", "0.00", "9750.00"], false),
            ("Jordan", "in", ded, ["1150.00", "164.55", "985.45"], false),
            ("Jordan", "in", oop, ["5200.00", "246.55", "4953.45"], false),
            ("Jordan", "out", ded, ["2450.00", "0.00", "2450.00"], false),
            ("Jordan", "out", oop, ["9750.00", "0.00", "9750.00"], false),
            ("Sam", "in", ded, ["1150.00", "262.80", "887.20"], false),
            ("Sam", "in", oop, ["5200.00", "537.93", "4662.07"], false),
            ("Sam", "out", ded, ["2450.00", "110.80", "2339.20"], false),
            ("Sam", "out", oop, ["9750.00", "133.60", "9616.40"], false),
            ("Family", "in", ded, ["2300.00", "1577.35", "722.65"], false),
            ("Family", "in", oop, ["10400.00", "2677.92", "7722.08"], false),
            ("Family", "out", ded, ["4900.00", "110.80", "4789.20"], false),
            ("Family", "out", oop, ["19500.00", "133.60", "19366.40"], false),
        ]
        .map(|(who, net, limit, money, met)| (who, net, limit, money.map(usd), met));
        assert_eq!(rows, expected, "medical only: no dental or pharmacy maximum");
        for r in &b.balances {
            assert_eq!(r.family, r.label == "Family");
            let patient = if r.family {
                family_id(MEMBER)
            } else {
                patient_id(MEMBER, &r.label)
            };
            assert_eq!(r.patient, patient);
            assert_eq!(r.as_of.as_deref(), Some(TODAY));
            assert_eq!(r.document, None);
        }
        assert_eq!(b.members.len(), 3);
    }

    #[test]
    fn a_calendar_year_plan_counts_the_year_it_is_read_in() {
        for (today, year) in [(TODAY, 2026), ("2026-12-31", 2026), ("2027-01-01", 2027)] {
            let b = balances(answer!("spending.json"), today).unwrap();
            // Alex's coverage took effect in 2023, Sam's in March 2026.
            assert_eq!(
                b.members[0].site.as_ref().unwrap().plan_start.as_deref(),
                Some("2023-04-01")
            );
            let everyone: Vec<&str> = b.members.iter().map(|m| m.id.as_str()).collect();
            assert_eq!(b.calendar_year, everyone);
            let period = BenefitPeriod {
                start: format!("{year}-01-01"),
                end: format!("{year}-12-31"),
            };
            assert_eq!(b.balances.len(), 16);
            for r in &b.balances {
                assert_eq!(r.plan_year, Some(year), "{today} {}", r.label);
                assert_eq!(r.period.as_ref(), Some(&period), "{today} {}", r.label);
            }
        }
    }

    #[test]
    fn without_the_calendar_year_mark_there_is_no_period() {
        let mut data = answer!("spending.json");
        each_benefit(&mut data, |_, b| b["benefitsPeriodIndicator"] = Value::Null);
        // Another coverage's mark is not the medical plan's.
        each_benefit(&mut data, |_, b| {
            if b["coverageType"] != "MEDICAL" {
                b["benefitsPeriodIndicator"] = "CALENDAR_YEAR".into();
            }
        });
        let b = balances(data, TODAY).unwrap();
        assert!(b.calendar_year.is_empty());
        assert_eq!(b.balances.len(), 16);
        for r in &b.balances {
            assert_eq!(r.period, None);
            // The year the member's coverage took effect; the family's rows
            // are read with the first member's.
            let year = match r.label.as_str() {
                "Alex" | "Family" => 2023,
                _ => 2026,
            };
            assert_eq!(r.plan_year, Some(year), "{}", r.label);
        }
    }

    /// The site marks both the maximums and the progress: either is enough.
    #[test]
    fn a_mark_on_the_maximums_or_the_progress_is_enough() {
        for keep in ["benefits", "accumulators"] {
            let mut data = answer!("spending.json");
            each_benefit(&mut data, |list, b| {
                if list != keep {
                    b["benefitsPeriodIndicator"] = Value::Null;
                }
            });
            assert_eq!(balances(data, TODAY).unwrap().calendar_year.len(), 3, "{keep}");
        }
    }

    /// One member's mark is that member's alone.
    #[test]
    fn a_member_without_the_mark_has_no_period() {
        let mut data = answer!("spending.json");
        for (list, inner) in [
            ("benefits", "benefitsResponses"),
            ("accumulators", "accumulatorSummaries"),
        ] {
            for dep in data["spendingOverviews"][list].as_array_mut().unwrap() {
                if dep["depSeqNbr"] == "01" {
                    for b in dep[inner][0]["benefits"].as_array_mut().unwrap() {
                        b["benefitsPeriodIndicator"] = Value::Null;
                    }
                }
            }
        }
        let b = balances(data, TODAY).unwrap();
        assert_eq!(
            b.calendar_year,
            [patient_id(MEMBER, "ALEX"), patient_id(MEMBER, "JORDAN")]
        );
        for r in &b.balances {
            assert_eq!(r.period.is_none(), r.label == "Sam", "{}", r.label);
        }
    }
}
