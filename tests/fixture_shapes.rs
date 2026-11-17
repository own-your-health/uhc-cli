//! The statement parser against the synthetic statements in
//! `tests/fixtures/eob/`, which follow UHC's STD-EOB layout (see
//! `docs/api.md`). Each assertion names a field a command reads, so a layout
//! the parser stops understanding fails here rather than emptying a column.

use uhc::cents::Cents;
use uhc::eob::{self, Issue, Limit, Network, Whom};

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

/// UHC's own layout: one "Claim detail for" block holds several claims,
/// each opening with its "Provider:" line, one of them the pharmacy's and
/// two of them the same provider's, and the last continues across a page.
#[test]
fn a_block_of_several_claims_reads_each_one() {
    let st = eob::parse(&fixture("2026-08-20-statement.txt"));
    assert!(st.issues.is_empty(), "{:?}", st.issues);
    let numbers: Vec<_> = st.claims.iter().map(|c| c.claim_number.as_deref()).collect();
    assert_eq!(
        numbers,
        [Some("SYNC00000006"), Some("SYNC00000007"), Some("SYNC00000008")]
    );
    assert!(st.claims.iter().all(|c| c.patient == "SAM"));
    let pharmacy = &st.claims[1];
    assert_eq!(pharmacy.provider.as_deref(), Some("Pharmacy"));
    assert_eq!(pharmacy.account_number.as_deref(), Some("00000000000000000"));
    assert_eq!(pharmacy.network, None, "the pharmacy's claim prints no status");
    assert_eq!(pharmacy.lines.len(), 1);
    assert_eq!(pharmacy.total.unwrap().you_owe, usd("$12.50"));
    let continued = &st.claims[2];
    assert_eq!(continued.provider.as_deref(), Some("EXAMPLE CLINIC"));
    assert_eq!(continued.lines.len(), 2, "the page break continues the claim");
    assert_eq!(continued.total.unwrap().billed, usd("$520.00"));
    assert_eq!(st.total_owed, Some(usd("$147.50")));
}

/// A page headed "Claim detail for SAM continued" that holds only the
/// processing codes opens no claim.
#[test]
fn a_continued_heading_over_the_codes_opens_no_claim() {
    let text = fixture("2026-08-20-statement.txt").replacen(
        "   Explanation of your claim processing codes\n",
        "   Claim detail for SAM continued\n   Explanation of your claim processing codes\n",
        1,
    );
    let st = eob::parse(&text);
    assert!(st.issues.is_empty(), "{:?}", st.issues);
    assert_eq!(st.claims.len(), 3);
}
