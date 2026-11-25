//! The statement parser against the synthetic statements in
//! `tests/fixtures/eob/`, which follow UHC's STD-EOB layout (see
//! `docs/api.md`). Each assertion names a field a command reads, so a layout
//! the parser stops understanding fails here rather than emptying a column.

use std::path::PathBuf;

use uhc::cents::Cents;
use uhc::eob::{self, Issue, Limit, Network, Whom};
use uhc::model::{Book, ClaimKind, ClaimRecord, StatementFile};

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/eob/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn usd(s: &str) -> Cents {
    Cents::parse(s).unwrap()
}

#[test]
fn a_two_member_statement_reads_header_balances_and_claims() {
    let text = fixture("2026-03-14-statement.txt");
    assert!(eob::is_uhc_eob(&text));
    let st = eob::parse(&text);
    assert!(st.synthetic);
    assert!(st.issues.is_empty(), "{:?}", st.issues);
    assert_eq!(st.statement_date.as_deref(), Some("2026-03-14"));
    assert_eq!(st.member.member_id.as_deref(), Some("SYNM000001"));
    assert_eq!(st.member.relationship.as_deref(), Some("EE"));
    assert_eq!(st.member.group_number.as_deref(), Some("SYNG0001"));
    assert_eq!(st.member.group_name.as_deref(), Some("EXAMPLE EMPLOYER"));

    let whom: Vec<&Whom> = st.balances.iter().map(|b| &b.whom).collect();
    assert_eq!(
        whom,
        [
            &Whom::Person("ALEX".into()),
            &Whom::Person("JORDAN".into()),
            &Whom::Family
        ]
    );
    for table in &st.balances {
        assert_eq!(table.plan_year, Some(2026));
        assert_eq!(table.rows.len(), 4);
        for r in &table.rows {
            assert_eq!(r.maximum.0 - r.used.0, r.remaining.0, "{r:?}");
        }
    }

    assert_eq!(st.claims.len(), 2);
    let alex = &st.claims[0];
    assert_eq!(alex.patient, "ALEX");
    assert_eq!(alex.claim_number.as_deref(), Some("SYNC00000001"));
    assert_eq!(alex.network, Some(Network::In));
    assert_eq!(alex.lines.len(), 2);
    let total = alex.total.unwrap();
    assert_eq!(total.billed, usd("$310.00"));
    assert_eq!(total.you_owe, usd("$185.00"));
    let jordan = &st.claims[1];
    assert_eq!(jordan.patient, "JORDAN");
    assert_eq!(jordan.provider.as_deref(), Some("TEST PEDIATRICS"));
    assert_eq!(jordan.total.unwrap().you_owe, Cents(0));
    assert!(st.codes.contains_key("UG"));
}

#[test]
fn an_out_of_network_claim_reads_a_wrapped_line_a_date_range_and_met() {
    let st = eob::parse(&fixture("2026-04-30-statement.txt"));
    assert!(st.issues.is_empty(), "{:?}", st.issues);
    assert_eq!(st.statement_date.as_deref(), Some("2026-04-30"));
    assert_eq!(st.service_period, Some(("2026-04-01".into(), "2026-04-03".into())));
    assert_eq!(st.provider_billed, Some(usd("$1,900.00")));
    assert_eq!(st.total_owed, Some(usd("$955.00")));

    let alex = &st.balances[0];
    let deductible = alex
        .rows
        .iter()
        .find(|r| r.network == Network::In && r.limit == Limit::Deductible)
        .unwrap();
    assert!(deductible.met);
    assert_eq!(deductible.remaining, Cents(0));
    let family = st.balances.iter().find(|b| b.whom == Whom::Family).unwrap();
    let oop = family
        .rows
        .iter()
        .find(|r| r.network == Network::Out && r.limit == Limit::OutOfPocket)
        .unwrap();
    assert_eq!(oop.maximum, usd("$16,000.00"));

    let c = &st.claims[0];
    assert_eq!(c.network, Some(Network::Out));
    assert_eq!(c.account_number.as_deref(), Some("SYNA0003"));
    let mri = &c.lines[0];
    assert_eq!(mri.description, "MRI WITHOUT CONTRAST");
    assert_eq!(mri.codes, ["2Q", "B1"]);
    assert_eq!(mri.date.as_deref(), Some("2026-04-01"));
    assert_eq!(mri.date_end.as_deref(), Some("2026-04-03"));
    let fee = &c.lines[1];
    assert_eq!(fee.description, "FACILITY FEE");
    assert_eq!(fee.amounts.not_covered, usd("$355.00"));
    assert_eq!(st.codes.len(), 2, "two codes printed side by side");
}

#[test]
fn a_spouse_statement_reads_a_copay() {
    let st = eob::parse(&fixture("2026-05-08-statement.txt"));
    assert!(st.issues.is_empty(), "{:?}", st.issues);
    assert_eq!(st.member.relationship.as_deref(), Some("SP"));
    let c = &st.claims[0];
    assert_eq!(c.claim_number.as_deref(), Some("SYNC00000004"));
    assert_eq!(c.total.unwrap().copay, usd("$25.00"));
}

#[test]
fn a_letter_that_mentions_eobs_is_not_a_statement() {
    let text = fixture("premium-notice.txt");
    assert!(!eob::is_uhc_eob(&text));
}

#[test]
fn a_damaged_statement_reports_each_problem_and_keeps_what_it_can() {
    let st = eob::parse(&fixture("damaged-statement.txt"));
    for issue in [
        Issue::NoMemberId,
        Issue::BalanceRowUnreadable,
        Issue::ClaimWithoutNumber,
        Issue::AmountsUnreadable,
        Issue::ClaimTotalMismatch,
        Issue::NoPlanYear,
    ] {
        assert!(st.issues.contains(&issue), "{issue:?} missing from {:?}", st.issues);
    }
    assert_eq!(st.claims.len(), 1);
    assert_eq!(st.claims[0].lines.len(), 1, "the line short of an amount is dropped");
    assert_eq!(
        st.statement_date.as_deref(),
        Some("2026-06-01"),
        "falls back to the balances' As of"
    );
}

#[test]
fn a_claim_continued_across_a_page_break_stays_one_claim() {
    let st = eob::parse(&fixture("2026-07-10-continued-statement.txt"));
    assert!(st.issues.is_empty(), "{:?}", st.issues);
    assert_eq!(st.claims.len(), 1, "the repeated header continues the claim");
    let c = &st.claims[0];
    assert_eq!(c.claim_number.as_deref(), Some("SYNC00000005"));
    let descriptions: Vec<&str> = c.lines.iter().map(|l| l.description.as_str()).collect();
    assert_eq!(
        descriptions,
        [
            "EMERGENCY ROOM VISIT HIGH COMPLEXITY",
            "LAB PANEL",
            "BILLING ADJUSTMENT FOR A PRIOR OVERPAYMENT"
        ],
        "wrapped text above and below the amounts is kept"
    );
    let adjustment = &c.lines[2].amounts;
    assert_eq!(adjustment.you_owe, Cents(-1000), "`- $10.00` is a credit");
    assert_eq!(adjustment.not_covered, Cents(-1000));
    let total = c.total.unwrap();
    assert_eq!(total.you_owe, usd("$70.00"));
    assert_eq!(st.total_owed, Some(usd("$70.00")));
}

#[test]
fn a_repeated_header_naming_another_claim_starts_it() {
    // The first block lacks its total; the next names a different claim.
    let text = fixture("2026-07-10-continued-statement.txt").replacen(
        "Claim number: SYNC00000005\n\n   LAB",
        "Claim number: SYNC00000006\n\n   LAB",
        1,
    );
    let st = eob::parse(&text);
    let numbers: Vec<_> = st.claims.iter().map(|c| c.claim_number.as_deref()).collect();
    assert_eq!(numbers, [Some("SYNC00000005"), Some("SYNC00000006")]);
    assert!(st.issues.contains(&Issue::ClaimWithoutTotal));
}

#[test]
fn unusual_whitespace_does_not_panic() {
    let text = fixture("2026-05-08-statement.txt")
        .replace("UG -- ", "UG\u{1680}-- ")
        .replace("   SPECIALIST", "\u{1680}\u{00a0} SPECIALIST");
    let st = eob::parse(&text);
    assert_eq!(st.claims.len(), 1);
}

/// One block of three claims.
const BLOCK: &str = "2026-05-07-statement.txt";
/// A pharmacy benefit's claim.
const PHARMACY: &str = "2026-04-15-statement.txt";
/// A claim of two pages, then a page of codes.
const PAGES: &str = "2026-10-09-statement.txt";

fn claims_of(text: &str) -> Vec<ClaimRecord> {
    let file = StatementFile::new(PathBuf::from("statement.pdf"), text.as_bytes(), eob::parse(text));
    Book::new(vec![file]).claims()
}

/// Each reads without an issue, and its claims make up its own summary.
#[test]
fn each_statement_reads_whole() {
    for (name, date, member, relationship, period) in [
        (BLOCK, "2026-05-07", "SAM", "SP", ("2026-04-07", "2026-04-25")),
        (PHARMACY, "2026-04-15", "ALEX", "EE", ("2026-04-02", "2026-04-03")),
        (PAGES, "2026-10-09", "JORDAN", "CH", ("2026-09-03", "2026-09-24")),
    ] {
        let text = fixture(name);
        assert!(eob::is_uhc_eob(&text), "{name}");
        let st = eob::parse(&text);
        assert!(st.issues.is_empty(), "{name}: {:?}", st.issues);
        assert_eq!(st.statement_date.as_deref(), Some(date));
        assert_eq!(st.member.name, Some(format!("{member} EXAMPLE")));
        assert_eq!(st.member.member_id.as_deref(), Some("SYNM000001"));
        assert_eq!(st.member.relationship.as_deref(), Some(relationship));
        assert_eq!(st.service_period, Some((period.0.into(), period.1.into())));
        assert!(st.claims.iter().all(|c| c.patient == member), "{name}");
        let sum = |f: fn(&eob::Amounts) -> Cents| -> Cents { st.claims.iter().map(|c| f(&c.total.unwrap())).sum() };
        assert_eq!(st.provider_billed, Some(sum(|a| a.billed)), "{name}");
        assert_eq!(st.total_owed, Some(sum(|a| a.you_owe)), "{name}");
        for c in &st.claims {
            let lines = c.lines.iter().fold(eob::Amounts::default(), |a, l| a + l.amounts);
            assert_eq!(c.total, Some(lines), "{name} {:?}", c.claim_number);
        }
        let whom: Vec<&Whom> = st.balances.iter().map(|b| &b.whom).collect();
        assert_eq!(whom, [&Whom::Person(member.into()), &Whom::Family], "{name}");
        for r in st.balances.iter().flat_map(|b| &b.rows) {
            assert_eq!(r.maximum.0 - r.used.0, r.remaining.0, "{name} {r:?}");
        }
    }
}

/// One "Claim detail for" block holds a claim for each "Provider:" line.
#[test]
fn a_block_holds_a_claim_for_each_provider_line() {
    let text = fixture(BLOCK);
    assert_eq!(text.matches("Claim detail for").count(), 1);
    let st = eob::parse(&text);
    let rows: Vec<_> = st
        .claims
        .iter()
        .map(|c| {
            let total = c.total.unwrap();
            (
                c.provider.as_deref().unwrap(),
                c.claim_number.as_deref().unwrap(),
                c.account_number.as_deref().unwrap(),
                c.network.unwrap(),
                c.lines.len(),
                total.billed,
                total.you_owe,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "EXAMPLE CHIROPRACTIC",
                "SYNC55526407",
                "SYNA0031",
                Network::Out,
                3,
                usd("$323.00"),
                usd("$288.80")
            ),
            (
                "EXAMPLE URGENT CARE",
                "SYNC55527315",
                "SYNA0032",
                Network::In,
                2,
                usd("$259.00"),
                usd("$65.00")
            ),
            (
                "EXAMPLE PHYSICAL THERAPY GROUP",
                "SYNC55520613",
                "SYNA0033",
                Network::In,
                4,
                usd("$547.00"),
                usd("$143.90")
            ),
        ]
    );
    // A line with no code, and the codes after the last claim.
    assert!(st.claims[2].lines[3].codes.is_empty());
    assert_eq!(st.codes.keys().collect::<Vec<_>>(), ["H2", "J6", "K4"]);
}

/// A claim goes on under the next page's heading, which repeats its provider
/// and claim number.
#[test]
fn a_claim_goes_on_under_the_next_pages_heading() {
    let text = fixture(PAGES);
    assert_eq!(text.matches("Claim number: SYNC55531742").count(), 2);
    let st = eob::parse(&text);
    assert_eq!(st.claims.len(), 1);
    let c = &st.claims[0];
    assert_eq!(c.claim_number.as_deref(), Some("SYNC55531742"));
    assert_eq!(c.account_number.as_deref(), Some("SYNA0051"), "the first page's");
    assert_eq!(c.network, Some(Network::In));
    let lines: Vec<_> = c
        .lines
        .iter()
        .map(|l| (l.description.as_str(), l.codes.join(" "), l.date.as_deref().unwrap()))
        .collect();
    let row = |words, codes: &str, date| (words, codes.to_string(), date);
    assert_eq!(
        lines,
        [
            row("SPEECH EVALUATION", "K4", "2026-09-03"),
            row("SPEECH THERAPY", "M3 K4", "2026-09-10"),
            row("SPEECH THERAPY", "M3 K4", "2026-09-17"),
            row("GROUP SPEECH THERAPY", "M3 K4", "2026-09-24"),
            row("SPEECH SCREENING", "H2 K4", "2026-09-24"),
        ]
    );
    assert_eq!(c.total.unwrap().copay, usd("$82.00"), "both pages' lines");
}

/// A page headed "Claim detail for X continued" that holds only the
/// processing codes opens no claim, and its codes are read.
#[test]
fn a_continued_heading_over_processing_codes_opens_no_claim() {
    let text = fixture(PAGES);
    let heading = "   Claim detail for JORDAN continued\n";
    assert_eq!(text.matches(heading).count(), 1);
    let st = eob::parse(&text);
    assert_eq!(st.claims.len(), 1);
    assert!(st.issues.is_empty(), "{:?}", st.issues);
    assert_eq!(st.codes.keys().collect::<Vec<_>>(), ["H2", "K4", "M3"]);
    assert_eq!(st.codes["H2"], "YOUR PLAN DOES NOT COVER THIS SERVICE OR ITEM.");
    assert_eq!(eob::parse(&text.replace(heading, "")), st, "as if it were not there");
}

/// A claim from "Provider: Pharmacy" is named by the first 17 digits of its
/// pharmacy claim number, which the statement prints as its "Patient account
/// number"; its claim number is the statement's own.
#[test]
fn a_pharmacy_claim_is_named_by_its_17_digit_account_number() {
    let text = fixture(PHARMACY);
    let st = eob::parse(&text);
    assert_eq!(st.claims[0].provider.as_deref(), Some("Pharmacy"));
    assert_eq!(st.claims[0].account_number.as_deref(), Some("55500000000000237"));
    assert_eq!(st.claims[0].lines[0].description, "FLUTICASONE SPRAY");
    assert!(st.claims[0].lines[0].codes.is_empty());

    let claims = claims_of(&text);
    let rows: Vec<_> = claims
        .iter()
        .map(|c| (c.id.as_str(), c.claim_number.as_deref().unwrap(), c.kind))
        .collect();
    assert_eq!(
        rows,
        [
            ("rx55500000000000237", "SYNC55519048", ClaimKind::Pharmacy),
            ("cSYNC55511903", "SYNC55511903", ClaimKind::Medical),
        ],
        "newest service first"
    );
    // A number of another shape, or another provider's, names no pharmacy
    // claim: it keeps the statement's claim number.
    for other in [
        text.replace("55500000000000237", "5550000000000023"),
        text.replace("Provider: Pharmacy  ", "Provider: EXAMPLE DRUG STORE  "),
    ] {
        assert_ne!(other, text);
        let claims = claims_of(&other);
        let c = claims.iter().find(|c| c.id == "cSYNC55519048").unwrap();
        assert_eq!(c.kind, ClaimKind::Medical);
        assert_eq!(claims.len(), 2);
    }
}
