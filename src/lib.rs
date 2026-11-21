//! `uhc`: UnitedHealthcare coverage, claims, EOBs and deductible progress for
//! the household medical-records system (health-records/v1).

pub mod app;
pub mod auth;
pub mod cents;
pub mod cli;
pub mod client;
pub mod eob;
pub mod fhir;
pub mod files;
pub mod grants;
pub mod model;
pub mod portal;
pub mod session;
pub mod shape;
pub mod site;
pub mod site_cmd;
pub mod source;
pub mod synth;
