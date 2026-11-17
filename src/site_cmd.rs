//! The commands that read the member site (`--profile <NAME>`), producing
//! what the files source produces for the same command: the same schemas,
//! ids and FHIR, with what the site adds (procedure and remark codes,
//! claim types, network, the members' birth dates and plan periods).
//!
//! [`run`] takes an [`Env`]: where sessions are kept and which site is
//! read. The binary's is the vault and member.uhc.com one second apart; the
//! tests' is an in-memory vault and a loopback server. Every document goes
//! to `out`; the session is written back once the command has done what it
//! was asked, never after an error or an incomplete export.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pk_cli_config::profile::ProfileName;
use pk_cli_core::{output, CliError, Paged};
use pk_cli_documents::{fs_safe, Document, DownloadBatch, SavedDocument, SkippedDocument};
use pk_med_records::{RawResource, RecordsWriter, Skip};
use serde_json::Value;

use crate::app::{
    balance_json, claim_json, context, export_args, find_member, header, incomplete_exit, iso, now_rfc3339,
    write_private, PatientRow, Window, BALANCE_COLUMNS, CLAIM_COLUMNS, PATIENT_COLUMNS,
};
use crate::cli::*;
use crate::fhir;
use crate::model::{ClaimRecord, Member, Origin, SiteDocument};
use crate::portal::{self, Household, Site};
use crate::session::SessionStore;

/// `Skip.code` for a claim the plan has not processed: its id changes once
/// it is.
const IN_PROCESS: &str = "in_process";

/// Where the site commands keep sessions and what they read.
pub struct Env {
    pub sessions: SessionStore,
    /// The site's origin, or a test's loopback server.
    pub base: String,
    /// The least time between one answer and the next request.
    pub pace: Duration,
}

impl Env {
    /// The binary's: the vault, member.uhc.com, one request a second.
    pub fn for_binary() -> Result<Env, CliError> {
        Ok(Env {
            sessions: SessionStore::open()?,
            base: crate::client::BASE.into(),
            pace: crate::client::PACE,
        })
    }
}

struct Ctx<'a> {
    cli: &'a Cli,
    site: Site<'a>,
}

impl Ctx<'_> {
    fn json(&self) -> bool {
        self.cli.common.json
    }

    fn note(&self, msg: &str) {
        if !self.cli.common.quiet {
            eprintln!("{msg}");
        }
    }
}

fn put(out: &mut dyn Write, v: &Value) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string());
    writeln!(out, "{text}").map_err(|e| CliError::Other(format!("writing the output: {e}")))
}

/// `--json`'s list document to `out`, else a table on stdout.
fn emit_list(
    ctx: &Ctx,
    out: &mut dyn Write,
    record: &str,
    items: Vec<Value>,
    columns: &[&str],
    empty: &str,
) -> Result<(), CliError> {
    if ctx.json() {
        return put(
            out,
            &serde_json::to_value(Paged::new(record, items)).unwrap_or_default(),
        );
    }
    if items.is_empty() {
        ctx.note(empty);
    } else {
        output::table(&output::table_view(&items, columns));
    }
    Ok(())
}

/// One member-site command as `profile`, its documents to `out`. The exit
/// code is not 0 only for an export whose stream ended incomplete (the
/// trailer carries the error).
pub fn run(cli: &Cli, env: &Env, profile: &ProfileName, out: &mut dyn Write) -> Result<i32, CliError> {
    if let Command::Records(RecordsCmd::Export(args)) = &cli.command {
        export_args(cli, args)?;
        if args.output.is_some() {
            return Err(CliError::Usage(
                "-o writes records to a plain file, for folders of synthetic statements only; the member site's \
                 are real"
                    .into(),
            )
            .with_remedy(format!(
                "pipe them into medrec: `{BIN} --profile {profile} records export --patient <ID> --ndjson | medrec \
                 import --from uhc`"
            )));
        }
    }
    let site = Site::open(&env.sessions, profile, &env.base, env.pace)?;
    let ctx = Ctx { cli, site };
    let code = match &cli.command {
        Command::Patients(PatientsCmd::List) => patients(&ctx, out).map(|()| 0),
        Command::Claims(ClaimsCmd::List(args)) => claims_list(&ctx, args, out).map(|()| 0),
        Command::Claims(ClaimsCmd::Get { claim }) => claims_get(&ctx, claim, out).map(|()| 0),
        Command::Accumulators(args) => accumulators(&ctx, args, out).map(|()| 0),
        Command::Documents(DocumentsCmd::List(range)) => documents_list(&ctx, range, out).map(|()| 0),
        Command::Documents(DocumentsCmd::Download(args)) => documents_download(&ctx, args, out).map(|()| 0),
        Command::Records(RecordsCmd::Export(args)) => export(&ctx, args, out),
        _ => Err(CliError::Other("not a member-site command".into())),
    }?;
    // Only after the command did what it was asked: an export that ended
    // incomplete may have ended on a refused session.
    if code == 0 && ctx.site.finish()? && cli.common.verbose {
        eprintln!("the session's cookies were updated");
    }
    Ok(code)
}

/// [`run`] for a command line, resolving its source as the binary does:
/// for the in-process tests.
pub fn run_args(cli: &Cli, env: &Env, out: &mut dyn Write) -> Result<i32, CliError> {
    let store = pk_cli_config::ConfigStore::new(BIN).with_override(cli.config.clone());
    let profiles = store.load::<crate::app::Config>()?.profiles;
    match crate::source::source(cli, &profiles)? {
        crate::source::Source::Site(profile) => run(cli, env, &profile, out),
        crate::source::Source::Files(_) => Err(CliError::Usage("a member-site command takes --profile".into())),
    }
}

fn household_with_documents(ctx: &Ctx) -> Result<(Household, Vec<SiteDocument>), CliError> {
    let mut house = ctx.site.household()?;
    let docs = ctx.site.documents(&house)?;
    portal::link_documents(&mut house, &docs);
    Ok((house, docs))
}

fn member<'a>(members: &'a [Member], q: &str) -> Result<&'a Member, CliError> {
    if members.is_empty() {
        return Err(CliError::NotFound(
            "the member site lists no member for this profile".into(),
        ));
    }
    find_member(members, q)
}

// -------------------------------------------------------------- patients

fn patients(ctx: &Ctx, out: &mut dyn Write) -> Result<(), CliError> {
    let (house, _) = household_with_documents(ctx)?;
    let rows: Vec<Value> = house
        .members
        .iter()
        .map(|m| {
            let row = PatientRow {
                context: context(m, m.site.as_ref().map(|s| s.full_name())),
                member_id: m.member_id.clone(),
                coverage_relationship: m.relationship.clone(),
                group_number: m.group_number.clone(),
                group_name: m.group_name.clone(),
                claims: m.claims,
                statements: m.statements,
                latest_statement: m.latest_statement.clone(),
            };
            serde_json::to_value(row).unwrap_or_default()
        })
        .collect();
    emit_list(
        ctx,
        out,
        "patient",
        rows,
        PATIENT_COLUMNS,
        "the member site lists no member",
    )
}

// ---------------------------------------------------------------- claims

fn claims_list(ctx: &Ctx, args: &ClaimsListArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let window = Window::of(&args.range)?;
    let house = ctx.site.household()?;
    let patient = args.patient.as_deref().map(|q| member(&house.members, q)).transpose()?;
    let claims: Vec<Value> = house
        .claims
        .iter()
        .filter(|c| patient.is_none_or(|m| c.patient == m.id))
        .filter(|c| window.contains(c.service_start.as_deref().or(c.processed.as_deref())))
        .take(args.range.limit.map(|n| n as usize).unwrap_or(usize::MAX))
        .map(|c| claim_json(c, false))
        .collect();
    emit_list(ctx, out, "claim", claims, CLAIM_COLUMNS, "no claims")
}

fn claims_get(ctx: &Ctx, q: &str, out: &mut dyn Write) -> Result<(), CliError> {
    let (house, _) = household_with_documents(ctx)?;
    let q = q.trim();
    let mut c = house
        .claims
        .iter()
        .find(|c| c.id == q || c.claim_number.as_deref() == Some(q))
        .cloned()
        .ok_or_else(|| CliError::NotFound(format!("no claim {q} on the member site (`uhc claims list`)")))?;
    ctx.site.detail(&house, &mut c)?;
    let v = claim_json(&c, true);
    if ctx.json() {
        put(out, &output::tagged("claim", v))
    } else {
        output::render(&v);
        Ok(())
    }
}

// ---------------------------------------------------------- accumulators

fn accumulators(ctx: &Ctx, args: &AccumulatorsArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let (members, rows) = ctx.site.balances()?;
    let patient = args.patient.as_deref().map(|q| member(&members, q)).transpose()?;
    let items: Vec<Value> = rows
        .iter()
        .filter(|b| args.year.is_none_or(|y| b.plan_year == Some(y)))
        .filter(|b| match patient {
            Some(m) => b.patient == m.id || (b.family && b.member_id == m.member_id),
            None => true,
        })
        .map(balance_json)
        .collect();
    emit_list(
        ctx,
        out,
        "accumulator",
        items,
        BALANCE_COLUMNS,
        "no plan balances on the member site",
    )
}

// ------------------------------------------------------------- documents

/// A site document's `documents/v1` row: one per document id, its id the
/// export's `DocumentReference` id when it is addressed to a member.
fn document_row(d: &SiteDocument) -> Document {
    let id = match &d.patient {
        Some(p) => fhir::document_id(&d.id, p),
        None => d.id.clone(),
    };
    let what = d.name.clone().unwrap_or_else(|| d.doc_type.clone());
    let mut name = match &d.date {
        Some(date) => format!("UnitedHealthcare {what} {date}"),
        None => format!("UnitedHealthcare {what}"),
    };
    if let Some(label) = &d.label {
        name.push_str(&format!(", {label}"));
    }
    let mut doc = Document::new(id, name);
    doc.date = d.date.clone();
    doc.category = Some(d.category());
    doc.file = Some(file_name(d));
    doc
}

/// `<date>_uhc-<category>_<document id>.pdf`: the same name each time, so a
/// re-run finds what it saved.
fn file_name(d: &SiteDocument) -> String {
    let stem = fs_safe(&format!(
        "{}_uhc-{}_{}",
        d.date.as_deref().unwrap_or("undated"),
        d.category(),
        d.id
    ));
    format!("{stem}.pdf")
}

fn documents_list(ctx: &Ctx, range: &pk_cli_core::RangeArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let window = Window::of(range)?;
    let (_, docs) = household_with_documents(ctx)?;
    let rows: Vec<Value> = docs
        .iter()
        .filter(|d| window.contains(d.date.as_deref()))
        .take(range.limit.map(|n| n as usize).unwrap_or(usize::MAX))
        .map(|d| serde_json::to_value(document_row(d)).unwrap_or_default())
        .collect();
    if ctx.json() {
        return put(
            out,
            &serde_json::to_value(Paged::new("document", rows)).unwrap_or_default(),
        );
    }
    if rows.is_empty() {
        ctx.note("no documents on the member site");
        return Ok(());
    }
    output::table(&output::table_view(&rows, &["date", "category", "name", "id"]));
    Ok(())
}

/// The document a `documents list` id names, all of it or a prefix of it or
/// of the site's document id (6 characters or more).
fn find_document<'a>(docs: &'a [SiteDocument], q: &str) -> Result<&'a SiteDocument, CliError> {
    let q = q.trim().to_ascii_lowercase();
    if q.len() < 6 {
        return Err(CliError::Usage("give at least 6 characters of the document id".into()));
    }
    if let Some(d) = docs.iter().find(|d| document_row(d).id == q || d.id == q) {
        return Ok(d);
    }
    let hits: Vec<&SiteDocument> = docs
        .iter()
        .filter(|d| document_row(d).id.starts_with(&q) || d.id.starts_with(&q))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(CliError::NotFound(format!("no document {q} (`uhc documents list`)"))),
        many => Err(CliError::NotFound(format!(
            "{q} matches {} documents; give more of the id",
            many.len()
        ))),
    }
}

fn documents_download(ctx: &Ctx, args: &DownloadArgs, out: &mut dyn Write) -> Result<(), CliError> {
    let (_, docs) = household_with_documents(ctx)?;
    if args.all {
        let dir = args.output.clone().unwrap_or_else(|| PathBuf::from("."));
        if dir.as_os_str() == "-" {
            return Err(CliError::Usage("--all writes a folder, not stdout".into()));
        }
        std::fs::create_dir_all(&dir).map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))?;
        let mut saved = Vec::new();
        let mut skipped = Vec::new();
        for d in &docs {
            let row = document_row(d);
            let path = dir.join(file_name(d));
            // The site's bytes differ on each download, so a file of this
            // name is this document saved before: not read again.
            if std::fs::symlink_metadata(&path).is_ok() && !args.overwrite {
                skipped.push(
                    SkippedDocument::new(row.id.clone(), "already saved; --overwrite replaces it").with_code("exists"),
                );
                continue;
            }
            let bytes = ctx.site.pdf(d)?;
            write_private(&path, &bytes, args.overwrite)?;
            saved.push(SavedDocument::from_document(
                &row,
                path.display().to_string(),
                bytes.len() as u64,
            ));
        }
        let kept = skipped.len();
        let batch = DownloadBatch::new(dir.display().to_string(), saved).with_skipped(skipped);
        if ctx.json() {
            return put(out, &serde_json::to_value(&batch).unwrap_or_default());
        }
        let mut note = format!("saved {} document(s) to {}", batch.count, batch.dir);
        if kept > 0 {
            note.push_str(&format!("; {kept} already there"));
        }
        ctx.note(&note);
        return Ok(());
    }
    let id = args.id.as_deref().expect("clap requires an id without --all");
    let d = find_document(&docs, id)?;
    let row = document_row(d);
    let target = match args.output.as_deref() {
        Some(p) if p.as_os_str() == "-" => None,
        Some(p) if p.is_dir() => Some(p.join(file_name(d))),
        Some(p) => Some(p.to_path_buf()),
        None => Some(PathBuf::from(file_name(d))),
    };
    // An existing file is refused before the download.
    if let Some(path) = &target {
        refuse_existing(path, args.overwrite)?;
    }
    let bytes = ctx.site.pdf(d)?;
    match target {
        None => out
            .write_all(&bytes)
            .map_err(|e| CliError::Other(format!("writing stdout: {e}"))),
        Some(path) => {
            write_private(&path, &bytes, args.overwrite)?;
            let saved = SavedDocument::from_document(&row, path.display().to_string(), bytes.len() as u64);
            if ctx.json() {
                put(out, &serde_json::to_value(&saved).unwrap_or_default())
            } else {
                ctx.note(&format!("saved {}", saved.path));
                Ok(())
            }
        }
    }
}

fn refuse_existing(path: &Path, overwrite: bool) -> Result<(), CliError> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Err(CliError::Usage(format!("{} is a folder", path.display()))),
        Ok(_) if !overwrite => Err(CliError::Usage(format!(
            "{} exists; pass --overwrite to replace it",
            path.display()
        ))),
        _ => Ok(()),
    }
}

// --------------------------------------------------------------- records

/// `records export --ndjson` from the site, streamed as it is read: the
/// member, the plan and the payer, each claim with its detail (with
/// `--since`, those processed on or after that day), then each document
/// addressed to the member (with `--since`, created on or after it) with
/// its PDF. A claim still in process is skipped (`in_process`): its id
/// changes once it is processed. A read that fails ends the stream
/// incomplete, the error in its trailer.
fn export(ctx: &Ctx, args: &ExportArgs, out: &mut dyn Write) -> Result<i32, CliError> {
    let since = args.since.as_deref().map(iso).transpose()?;
    let since = since.as_deref();
    let (house, docs) = household_with_documents(ctx)?;
    let m = member(&house.members, &args.who.patient)?.clone();
    let after = |d: Option<&str>| since.is_none_or(|s| d.is_some_and(|d| d >= s));
    let selected_docs: Vec<&SiteDocument> = docs
        .iter()
        .filter(|d| d.patient.as_deref() == Some(m.id.as_str()) && after(d.date.as_deref()))
        .collect();
    let exported_doc = |id: &str| selected_docs.iter().any(|d| d.id == id);

    let mut w = RecordsWriter::start(out, &header(&m, since))?;
    let mut claims: Vec<ClaimRecord> = Vec::new();
    for c in house.claims.iter().filter(|c| c.patient == m.id) {
        if c.status.as_deref() == Some(IN_PROCESS) {
            w.skip(Skip::resource(
                "ExplanationOfBenefit",
                &c.id,
                IN_PROCESS,
                format!(
                    "claim {} is still in process; it is exported once processed, under its final id",
                    c.id
                ),
            ));
            continue;
        }
        if after(c.processed.as_deref()) {
            claims.push(c.clone());
        }
    }
    let written = (|| -> Result<(), CliError> {
        let coverage = fhir::coverage_id(&m);
        let full_name = m.site.as_ref().map(|s| s.full_name());
        for r in [
            fhir::organization(Origin::Site),
            fhir::patient(&m, full_name.as_deref()),
            fhir::coverage(&m),
        ] {
            w.resource(&RawResource::from_value(&r)?)?;
        }
        for c in &mut claims {
            ctx.site.detail(&house, c)?;
            // A document reference only to a document this stream carries.
            if c.document.as_deref().is_some_and(|d| !exported_doc(d)) {
                c.document = None;
            }
            w.resource(&RawResource::from_value(&fhir::explanation_of_benefit(
                c,
                &coverage,
                &[],
            ))?)?;
        }
        for d in &selected_docs {
            let bytes = ctx.site.pdf(d)?;
            let related: Vec<&ClaimRecord> = claims.iter().filter(|c| c.documents.contains(&d.id)).collect();
            let doc_ref = fhir::site_document_reference(d, &m.id, &related, bytes.len() as u64);
            w.resource(&RawResource::from_value(&doc_ref)?)?;
            w.resource(&RawResource::from_value(&fhir::site_binary(d, &m.id, &bytes))?)?;
        }
        Ok(())
    })();
    let finished = match written {
        Ok(()) => w.finish(now_rfc3339(), None)?,
        // A session refused partway is the stream's error, and the command's.
        Err(e) => w.finish(now_rfc3339(), Some(&e))?,
    };
    Ok(if finished.trailer.complete {
        0
    } else {
        incomplete_exit(&finished.trailer)
    })
}
