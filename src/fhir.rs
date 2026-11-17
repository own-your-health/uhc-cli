//! Statements and member-site answers mapped into the FHIR shapes `medrec`
//! reads: CARIN Blue Button `ExplanationOfBenefit` and `Coverage`, with
//! `Patient`, the payer `Organization`, and each statement PDF or site
//! document as `DocumentReference` + `Binary`.
//!
//! The mapping follows CARIN BB 2.0's (STU2) element choices and code systems
//! where the source carries the value: the site adds procedure codes (CPT,
//! HCPCS, CDT), revenue codes, remark codes, the claim type and the network.
//! It does not claim the profiles (`meta.profile`): neither source gives the
//! diagnoses CARIN requires. Every resource says where it came from in
//! `meta.source` and a `meta.tag`.

use base64::Engine;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::cents::Cents;
use crate::eob::{Amounts, Limit, Network};
use crate::model::{
    hex, Balances, ClaimKind, ClaimRecord, Member, Origin, SiteDocument, StatementFile, ORGANIZATION, ORGANIZATION_NAME,
};

pub const META_SOURCE: &str = "uhc:eob-pdf";
pub const META_SOURCE_SITE: &str = "uhc:member-site";
pub const TAG_SYSTEM: &str = "https://github.com/piekstra/uhc-cli/fhir/tag";
pub const TAG_MAPPED: &str = "mapped-from-eob-statement";
pub const TAG_MAPPED_SITE: &str = "mapped-from-member-site";
const CLAIM_TYPE: &str = "http://terminology.hl7.org/CodeSystem/claim-type";
const CPT: &str = "http://www.ama-assn.org/go/cpt";
const HCPCS: &str = "https://www.cms.gov/Medicare/Coding/HCPCSReleaseCodeSets";
const CDT: &str = "http://ada.org/cdt";
const REVENUE: &str = "https://www.nubc.org/CodeSystem/RevenueCodes";
pub const MEMBER_ID_SYSTEM: &str = "https://www.uhc.com/fhir/member-id";
pub const CLAIM_NUMBER_SYSTEM: &str = "https://www.uhc.com/fhir/claim-number";
pub const BENEFIT_TYPE_SYSTEM: &str = "https://github.com/piekstra/uhc-cli/fhir/benefit-type";

const HL7_ADJUDICATION: &str = "http://terminology.hl7.org/CodeSystem/adjudication";
const C4BB_ADJUDICATION: &str = "http://hl7.org/fhir/us/carin-bb/CodeSystem/C4BBAdjudication";
const C4BB_SUPPORTING_INFO: &str = "http://hl7.org/fhir/us/carin-bb/CodeSystem/C4BBSupportingInfoType";
const C4BB_DISCRIMINATOR: &str = "http://hl7.org/fhir/us/carin-bb/CodeSystem/C4BBAdjudicationDiscriminator";
const C4BB_ADJUDICATION_STATUS: &str = "http://hl7.org/fhir/us/carin-bb/CodeSystem/C4BBPayerAdjudicationStatus";
const DATA_ABSENT_REASON: &str = "http://hl7.org/fhir/StructureDefinition/data-absent-reason";

/// A statement without a member ID gets a stand-in for grouping its claims
/// (`model::Book::member_id`); it is never exported as UHC's identifier.
fn printed_member_id(m: &Member) -> Option<&str> {
    (!m.member_id.starts_with(crate::model::UNKNOWN_MEMBER_PREFIX)).then_some(m.member_id.as_str())
}

/// A required element the statement does not print.
fn absent() -> Value {
    json!({ "extension": [{ "url": DATA_ABSENT_REASON, "valueCode": "unknown" }] })
}

fn meta(origin: Origin) -> Value {
    match origin {
        Origin::Statements => json!({
            "source": META_SOURCE,
            "tag": [{ "system": TAG_SYSTEM, "code": TAG_MAPPED, "display": "Mapped by uhc from a UnitedHealthcare EOB statement" }],
        }),
        Origin::Site => json!({
            "source": META_SOURCE_SITE,
            "tag": [{ "system": TAG_SYSTEM, "code": TAG_MAPPED_SITE, "display": "Mapped by uhc from the UnitedHealthcare member site" }],
        }),
    }
}

/// The code system of a procedure code, by its shape: CPT (five digits, or
/// four and `F`, `T` or `U`), CDT (`D` and four digits), HCPCS Level II (a
/// letter and four digits). `None` for anything else, which is then text.
pub fn procedure_system(code: &str) -> Option<&'static str> {
    let b = code.as_bytes();
    if b.len() != 5 {
        return None;
    }
    let tail_digits = b[1..].iter().all(u8::is_ascii_digit);
    if b[..4].iter().all(u8::is_ascii_digit) && (b[4].is_ascii_digit() || matches!(b[4], b'F' | b'T' | b'U')) {
        Some(CPT)
    } else if b[0] == b'D' && tail_digits {
        Some(CDT)
    } else if b[0].is_ascii_uppercase() && tail_digits {
        Some(HCPCS)
    } else {
        None
    }
}

/// CARIN's claim type for a claim, or text where the source does not say.
fn claim_type(kind: ClaimKind) -> Value {
    let (code, display, text) = match kind {
        ClaimKind::Medical => return json!({ "text": "Medical claim" }),
        ClaimKind::Professional => ("professional", "Professional", "Professional claim"),
        ClaimKind::Institutional => ("institutional", "Institutional", "Institutional claim"),
        ClaimKind::Pharmacy => ("pharmacy", "Pharmacy", "Pharmacy claim"),
        ClaimKind::Oral => ("oral", "Oral", "Dental claim"),
    };
    json!({ "coding": [{ "system": CLAIM_TYPE, "code": code, "display": display }], "text": text })
}

fn coding(system: &str, code: &str) -> Value {
    json!({ "coding": [{ "system": system, "code": code }] })
}

fn money(c: Cents) -> Value {
    json!({ "value": c.fhir_value(), "currency": "USD" })
}

pub fn organization(origin: Origin) -> Value {
    json!({
        "resourceType": "Organization",
        "id": ORGANIZATION,
        "meta": meta(origin),
        "active": true,
        "type": [coding("http://terminology.hl7.org/CodeSystem/organization-type", "pay")],
        "name": ORGANIZATION_NAME,
    })
}

/// `full_name` is the "Member/Patient" a statement addressed to this member
/// prints, when there is one, or the site's first and last name.
pub fn patient(m: &Member, full_name: Option<&str>) -> Value {
    let mut name = Map::new();
    if let Some(full) = full_name {
        name.insert("text".into(), full.into());
    }
    if let Some(last) = m.site.as_ref().and_then(|s| s.last_name.as_deref()) {
        name.insert("family".into(), last.into());
    }
    name.insert("given".into(), json!([m.label]));
    let mut v = json!({
        "resourceType": "Patient",
        "id": m.id,
        "meta": meta(m.origin),
    });
    if let Some(id) = printed_member_id(m) {
        v["identifier"] = json!([{
            "type": coding("http://terminology.hl7.org/CodeSystem/v2-0203", "MB"),
            "system": MEMBER_ID_SYSTEM,
            "value": id,
        }]);
    }
    v["name"] = json!([Value::Object(name)]);
    if let Some(site) = &m.site {
        if let Some(g) = &site.gender {
            v["gender"] = g.clone().into();
        }
        if let Some(d) = &site.birth_date {
            v["birthDate"] = d.clone().into();
        }
    }
    v
}

pub fn coverage_id(m: &Member) -> String {
    let key = format!("{}|{}|{}", m.member_id, m.id, m.group_number.as_deref().unwrap_or(""));
    format!("cov-{}", &hex(&Sha256::digest(key.as_bytes()))[..12])
}

pub fn coverage(m: &Member) -> Value {
    let mut v = json!({
        "resourceType": "Coverage",
        "id": coverage_id(m),
        "meta": meta(m.origin),
        "status": "active",
    });
    // The site's plan status; a statement implies an active plan.
    if let Some(status) = m.site.as_ref().and_then(|s| s.plan_status.as_deref()) {
        if !status.eq_ignore_ascii_case("active") {
            v["status"] = "cancelled".into();
        }
    }
    if m.group_number.is_some() {
        v["type"] = json!({ "text": "Group health plan" });
    }
    if let Some(id) = printed_member_id(m) {
        v["subscriberId"] = id.into();
    }
    v["beneficiary"] = json!({ "reference": format!("Patient/{}", m.id) });
    if let Some(rel) = &m.relationship {
        v["relationship"] = coding("http://terminology.hl7.org/CodeSystem/subscriber-relationship", rel);
    }
    if let Some(site) = &m.site {
        let mut period = Map::new();
        if let Some(s) = &site.plan_start {
            period.insert("start".into(), s.clone().into());
        }
        if let Some(e) = &site.plan_end {
            period.insert("end".into(), e.clone().into());
        }
        if !period.is_empty() {
            v["period"] = Value::Object(period);
        }
    }
    v["payor"] = json!([{ "reference": format!("Organization/{ORGANIZATION}"), "display": ORGANIZATION_NAME }]);
    if let Some(group) = &m.group_number {
        let mut class = json!({
            "type": coding("http://terminology.hl7.org/CodeSystem/coverage-class", "group"),
            "value": group,
        });
        if let Some(name) = &m.group_name {
            class["name"] = name.clone().into();
        }
        v["class"] = json!([class]);
    }
    v
}

/// The adjudication entries of a line or a claim: CARIN's categories, with
/// their code systems' display text.
fn adjudication(a: &Amounts) -> Vec<Value> {
    let rows: [(&str, &str, &str, Cents); 9] = [
        (HL7_ADJUDICATION, "submitted", "Submitted Amount", a.billed),
        (C4BB_ADJUDICATION, "discount", "Discount", a.saved),
        (HL7_ADJUDICATION, "eligible", "Eligible Amount", a.allowed),
        (HL7_ADJUDICATION, "benefit", "Benefit Amount", a.plan_paid),
        (HL7_ADJUDICATION, "deductible", "Deductible", a.deductible),
        (HL7_ADJUDICATION, "copay", "CoPay", a.copay),
        (C4BB_ADJUDICATION, "coinsurance", "Coinsurance", a.coinsurance),
        (C4BB_ADJUDICATION, "noncovered", "Noncovered", a.not_covered),
        (C4BB_ADJUDICATION, "memberliability", "Member liability", a.you_owe),
    ];
    rows.iter()
        .map(|(system, code, display, amount)| {
            json!({
                "category": { "coding": [{ "system": system, "code": code, "display": display }] },
                "amount": money(*amount),
            })
        })
        .collect()
}

/// A statement as one member's document. A statement carrying two members'
/// claims is a document of each, with its own subject, so the id names both.
pub fn document_id(file_id: &str, patient: &str) -> String {
    format!("doc-{file_id}-{patient}")
}

pub fn binary_id(file_id: &str, patient: &str) -> String {
    format!("bin-{file_id}-{patient}")
}

/// One claim. `balances` are the plan balances its statement printed for
/// this patient and the family.
pub fn explanation_of_benefit(c: &ClaimRecord, coverage: &str, balances: &[&Balances]) -> Value {
    let mut v = json!({
        "resourceType": "ExplanationOfBenefit",
        "id": c.id,
        "meta": meta(c.origin),
    });
    if let Some(num) = &c.claim_number {
        v["identifier"] = json!([{
            "type": coding("http://hl7.org/fhir/us/carin-bb/CodeSystem/C4BBIdentifierType", "uc"),
            "system": CLAIM_NUMBER_SYSTEM,
            "value": num,
        }]);
    }
    // A reversed pharmacy claim is cancelled; its amounts are the original's.
    v["status"] = if c.reversed { "cancelled" } else { "active" }.into();
    // A statement does not print whether a medical claim is professional or
    // institutional: then the type is text only.
    v["type"] = claim_type(c.kind);
    v["use"] = "claim".into();
    v["patient"] = json!({ "reference": format!("Patient/{}", c.patient) });
    if let Some(start) = &c.service_start {
        let mut period = json!({ "start": start });
        if let Some(end) = &c.service_end {
            period["end"] = end.clone().into();
        }
        v["billablePeriod"] = period;
    }
    // `created` and `provider` are required; a damaged statement may print
    // neither, and FHIR says so with data-absent-reason. On the site it is
    // the day the claim was processed.
    match c.statement_date.as_ref().or(c.processed.as_ref()) {
        Some(created) => v["created"] = created.clone().into(),
        None => v["_created"] = absent(),
    }
    v["insurer"] = json!({ "reference": format!("Organization/{ORGANIZATION}"), "display": ORGANIZATION_NAME });
    v["provider"] = match &c.claim.provider {
        Some(p) => json!({ "display": p }),
        None => absent(),
    };
    v["outcome"] = "complete".into();

    let mut info: Vec<Value> = c
        .document
        .iter()
        .map(|d| {
            json!({
                "sequence": 1,
                "category": coding("http://terminology.hl7.org/CodeSystem/claiminformationcategory", "info"),
                "valueReference": { "reference": format!("DocumentReference/{}", document_id(d, &c.patient)) },
            })
        })
        .collect();
    if let Some(acct) = &c.claim.account_number {
        info.push(json!({
            "sequence": info.len() + 1,
            "category": coding(C4BB_SUPPORTING_INFO, "patientaccountnumber"),
            "valueString": acct,
        }));
    }
    v["supportingInfo"] = Value::Array(info);
    v["insurance"] = json!([{ "focal": true, "coverage": { "reference": format!("Coverage/{coverage}") } }]);
    // CARIN BB STU2: the claim's network status is the header-level
    // `benefitpaymentstatus` adjudication.
    if let Some(network) = c.claim.network {
        let (code, display) = match network {
            Network::In => ("innetwork", "In Network"),
            Network::Out => ("outofnetwork", "Out Of Network"),
        };
        v["adjudication"] = json!([{
            "category": { "coding": [{ "system": C4BB_DISCRIMINATOR, "code": "benefitpaymentstatus", "display": "Benefit Payment Status" }] },
            "reason": { "coding": [{ "system": C4BB_ADJUDICATION_STATUS, "code": code, "display": display }] },
        }]);
    }

    let notes: Vec<&String> = c.codes.keys().collect();
    let items: Vec<Value> = c
        .claim
        .lines
        .iter()
        .enumerate()
        .map(|(n, l)| {
            let mut item = json!({ "sequence": n + 1 });
            if let Some(rev) = &l.revenue {
                let mut c = json!({ "system": REVENUE, "code": rev.code });
                if let Some(d) = &rev.display {
                    c["display"] = d.clone().into();
                }
                item["revenue"] = json!({ "coding": [c] });
            }
            item["productOrService"] = match &l.procedure {
                Some(p) => match procedure_system(&p.code) {
                    Some(system) => {
                        let mut coding = json!({ "system": system, "code": p.code });
                        if let Some(d) = &p.display {
                            coding["display"] = d.clone().into();
                        }
                        json!({ "coding": [coding], "text": l.description })
                    }
                    None => json!({ "text": format!("{} {}", p.code, l.description) }),
                },
                None => json!({ "text": l.description }),
            };
            match (&l.date, &l.date_end) {
                (Some(d), Some(e)) => item["servicedPeriod"] = json!({ "start": d, "end": e }),
                (Some(d), None) => item["servicedDate"] = d.clone().into(),
                _ => {}
            }
            let numbers: Vec<usize> = l
                .codes
                .iter()
                .filter_map(|code| notes.iter().position(|k| *k == code).map(|p| p + 1))
                .collect();
            if !numbers.is_empty() {
                item["noteNumber"] = json!(numbers);
            }
            if let Some(tooth) = &l.tooth {
                item["bodySite"] = json!({ "text": format!("Tooth {tooth}") });
            }
            item["net"] = money(l.amounts.billed);
            item["adjudication"] = Value::Array(adjudication(&l.amounts));
            item
        })
        .collect();
    if !items.is_empty() {
        v["item"] = Value::Array(items);
    }
    v["total"] = Value::Array(adjudication(&c.totals));
    if !notes.is_empty() {
        v["processNote"] = Value::Array(
            c.codes
                .iter()
                .enumerate()
                .map(|(n, (code, text))| json!({ "number": n + 1, "type": "print", "text": format!("{code} -- {text}") }))
                .collect(),
        );
    }
    if !balances.is_empty() {
        v["benefitBalance"] = Value::Array(benefit_balance(balances));
    }
    v
}

fn benefit_balance(balances: &[&Balances]) -> Vec<Value> {
    let mut groups: Vec<((Network, bool), Vec<Value>)> = Vec::new();
    for b in balances {
        let key = (b.network, b.family);
        let financial = json!({
            "type": match b.limit {
                Limit::Deductible => json!({ "coding": [{ "system": "http://terminology.hl7.org/CodeSystem/benefit-type", "code": "deductible" }], "text": "Deductible" }),
                Limit::OutOfPocket => json!({ "coding": [{ "system": BENEFIT_TYPE_SYSTEM, "code": "out-of-pocket-maximum" }], "text": "Out-of-pocket limit" }),
            },
            "allowedMoney": money(b.maximum),
            "usedMoney": money(b.used),
        });
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, f)) => f.push(financial),
            None => groups.push((key, vec![financial])),
        }
    }
    groups
        .into_iter()
        .map(|((network, family), financial)| {
            json!({
                "category": coding("http://terminology.hl7.org/CodeSystem/ex-benefitcategory", "1"),
                "network": coding("http://terminology.hl7.org/CodeSystem/benefit-network", network.code()),
                "unit": coding("http://terminology.hl7.org/CodeSystem/benefit-unit", if family { "family" } else { "individual" }),
                "term": coding("http://terminology.hl7.org/CodeSystem/benefit-term", "annual"),
                "financial": financial,
            })
        })
        .collect()
}

/// The statement PDF as a document of `patient`, related to its claims.
pub fn document_reference(f: &StatementFile, patient: &str, claims: &[&ClaimRecord]) -> Value {
    let st = &f.statement;
    let mut v = json!({
        "resourceType": "DocumentReference",
        "id": document_id(&f.id, patient),
        "meta": meta(Origin::Statements),
        "status": "current",
        "type": { "text": "Explanation of Benefits" },
        "category": [{ "text": "EOB" }],
        "subject": { "reference": format!("Patient/{patient}") },
    });
    if let Some(d) = &st.statement_date {
        v["date"] = format!("{d}T00:00:00Z").into();
    }
    v["description"] = match &st.statement_date {
        Some(d) => format!("UnitedHealthcare Explanation of Benefits, {d}"),
        None => "UnitedHealthcare Explanation of Benefits".to_string(),
    }
    .into();
    let mut attachment = json!({
        "contentType": "application/pdf",
        "url": format!("Binary/{}", binary_id(&f.id, patient)),
        "size": f.bytes,
        "title": match &st.statement_date {
            Some(d) => format!("UHC EOB {d}.pdf"),
            None => format!("UHC EOB {}.pdf", f.id),
        },
    });
    if let Some(d) = &st.statement_date {
        attachment["creation"] = d.clone().into();
    }
    v["content"] = json!([{ "attachment": attachment }]);
    let mut context = json!({
        "related": claims.iter().map(|c| json!({ "reference": format!("ExplanationOfBenefit/{}", c.id) })).collect::<Vec<_>>(),
    });
    if let Some((s, e)) = &st.service_period {
        context["period"] = json!({ "start": s, "end": e });
    }
    v["context"] = context;
    v
}

pub fn binary(f: &StatementFile, patient: &str, content: &[u8]) -> Value {
    json!({
        "resourceType": "Binary",
        "id": binary_id(&f.id, patient),
        "meta": meta(Origin::Statements),
        "contentType": "application/pdf",
        "data": base64::engine::general_purpose::STANDARD.encode(content),
    })
}

/// A member-site document as a document of `patient`, related to the
/// exported claims it lists. `size` is the PDF's length in bytes.
pub fn site_document_reference(doc: &SiteDocument, patient: &str, claims: &[&ClaimRecord], size: u64) -> Value {
    let mut v = json!({
        "resourceType": "DocumentReference",
        "id": document_id(&doc.id, patient),
        "meta": meta(Origin::Site),
        "status": "current",
        "type": { "text": doc.doc_type },
        "category": [{ "text": if doc.is_eob() { "EOB".to_string() } else { doc.doc_type.clone() } }],
        "subject": { "reference": format!("Patient/{patient}") },
    });
    if let Some(d) = &doc.date {
        v["date"] = format!("{d}T00:00:00Z").into();
    }
    let what = doc.name.as_deref().unwrap_or(&doc.doc_type);
    v["description"] = match &doc.date {
        Some(d) => format!("UnitedHealthcare {what}, {d}"),
        None => format!("UnitedHealthcare {what}"),
    }
    .into();
    let mut attachment = json!({
        "contentType": "application/pdf",
        "url": format!("Binary/{}", binary_id(&doc.id, patient)),
        "size": size,
        "title": match &doc.date {
            Some(d) => format!("UHC {what} {d}.pdf"),
            None => format!("UHC {what} {}.pdf", doc.id),
        },
    });
    if let Some(d) = &doc.date {
        attachment["creation"] = d.clone().into();
    }
    v["content"] = json!([{ "attachment": attachment }]);
    if !claims.is_empty() {
        v["context"] = json!({
            "related": claims.iter().map(|c| json!({ "reference": format!("ExplanationOfBenefit/{}", c.id) })).collect::<Vec<_>>(),
        });
    }
    v
}

pub fn site_binary(doc: &SiteDocument, patient: &str, content: &[u8]) -> Value {
    json!({
        "resourceType": "Binary",
        "id": binary_id(&doc.id, patient),
        "meta": meta(Origin::Site),
        "contentType": "application/pdf",
        "data": base64::engine::general_purpose::STANDARD.encode(content),
    })
}
