use crate::error::{self, Result};
use clap::Parser;
use log::{debug, info};
use serde::Deserialize;
use serde_plain::derive_fromstr_from_deserialize;
use snafu::ResultExt;
use testsys_model::test_manager::{CrdState, CrdType, SelectionParams, StatusColumn, TestManager};

/// Check the status of testsys objects.
#[derive(Debug, Parser)]
pub(crate) struct Status {
    /// Configure the output of the command (json, simplejson, condensed, narrow, wide).
    #[arg(long, short = 'o')]
    output: Option<StatusOutput>,

    /// Focus status on a particular arch
    #[arg(long)]
    arch: Option<String>,

    /// Focus status on a particular variant
    #[arg(long)]
    variant: Option<String>,

    /// Only show tests
    #[arg(long)]
    test: bool,

    /// Only show passed tests
    #[arg(long, conflicts_with_all=&["failed", "running"])]
    passed: bool,

    /// Only show failed tests
    #[arg(long, conflicts_with_all=&["passed", "running"])]
    failed: bool,

    /// Only CRD's that haven't finished
    #[arg(long, conflicts_with_all=&["passed", "failed"])]
    running: bool,
}

impl Status {
    pub(crate) async fn run(self, client: TestManager) -> Result<()> {
        let state = if self.running {
            Some(CrdState::NotFinished)
        } else if self.passed {
            Some(CrdState::Passed)
        } else if self.failed {
            Some(CrdState::Failed)
        } else {
            None
        };
        let crd_type = self.test.then_some(CrdType::Test);
        let mut labels = Vec::new();
        if let Some(arch) = self.arch {
            labels.push(format!("testsys/arch={}", arch))
        };
        if let Some(variant) = self.variant {
            labels.push(format!("testsys/variant={}", variant))
        };
        let mut status = client
            .status(&SelectionParams {
                labels: Some(labels.join(",")),
                state,
                crd_type,
                ..Default::default()
            })
            .await?;

        status.new_column("ARCH", |crd| {
            crd.labels()
                .get("testsys/arch")
                .cloned()
                .into_iter()
                .collect()
        });
        status.add_column(StatusColumn::name());
        status.add_column(StatusColumn::crd_type());
        status.add_column(StatusColumn::state());
        status.add_column(StatusColumn::passed());
        status.add_column(StatusColumn::failed());
        status.add_column(StatusColumn::skipped());

        /// ** TYPE  ARCH   VARIANT  TEST-TYPE   STATE   PASSED   FAILED
        status.add_column(StatusColumn::crd_type());
        status.new_column("TEST-TYPE", |crd| {
            crd.labels()
                .get("testsys/type")
                .cloned()
                .into_iter()
                .collect()
        });
        status.new_column("ARCH", |crd| {
            crd.labels()
                .get("testsys/arch")
                .cloned()
                .into_iter()
                .collect()
        });
        status.add_column(StatusColumn::passed());
        status.add_column(StatusColumn::failed());

        match self.output {
            Some(StatusOutput::Json) => {
                status.add_column(StatusColumn::name());
                status.add_column(StatusColumn::crd_type());
                status.add_column(StatusColumn::state());
                status.add_column(StatusColumn::passed());
                status.add_column(StatusColumn::failed());
                status.add_column(StatusColumn::skipped());
                info!(
                    "{}",
                    serde_json::to_string_pretty(&status).context(error::SerdeJsonSnafu {
                        what: "Could not create string from status."
                    })?
                );
                return Ok(());
            }
            Some(StatusOutput::Condensed) => {
                status.add_column(StatusColumn::crd_type());
                status.new_column("TEST-TYPE", |crd| {
                    crd.labels()
                        .get("testsys/type")
                        .cloned()
                        .into_iter()
                        .collect()
                });
                status.new_column("ARCH", |crd| {
                    crd.labels()
                        .get("testsys/arch")
                        .cloned()
                        .into_iter()
                        .collect()
                });
                status.add_column(StatusColumn::passed());
                status.add_column(StatusColumn::failed());
            }
            Some(StatusOutput::Narrow) => {
                status.add_column(StatusColumn::name());
                status.add_column(StatusColumn::crd_type());
                status.add_column(StatusColumn::state());
                status.add_column(StatusColumn::passed());
                status.add_column(StatusColumn::failed());
                status.add_column(StatusColumn::skipped());
            },
            None => {
                status.add_column(StatusColumn::name());
                status.add_column(StatusColumn::crd_type());
                status.add_column(StatusColumn::state());
                status.add_column(StatusColumn::passed());
                status.add_column(StatusColumn::failed());
                status.add_column(StatusColumn::skipped());
                status.new_column("BUILD ID", |crd| {
                    crd.labels()
                        .get("testsys/build-id")
                        .cloned()
                        .into_iter()
                        .collect()
                });
                status.add_column(StatusColumn::last_update());
            }
            Some(StatusOutput::Wide) => {
                status.add_column(StatusColumn::name());
                status.add_column(StatusColumn::crd_type());
                status.add_column(StatusColumn::state());
                status.add_column(StatusColumn::passed());
                status.add_column(StatusColumn::failed());
                status.add_column(StatusColumn::skipped());
                status.new_column("BUILD ID", |crd| {
                    crd.labels()
                        .get("testsys/build-id")
                        .cloned()
                        .into_iter()
                        .collect()
                });
                status.add_column(StatusColumn::last_update());
            }

        };

        let (width, _) = term_size::dimensions().unwrap_or((80, 0));
        debug!("Window width '{}'", width);
        println!("{:width$}", status);

        Ok(())
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
enum StatusOutput {
    /// Output the status in json
    Json,
    /// Output the simplified status in json
    SimpleJson,
    /// Show condensed and simplified status table
    Condensed,
    /// Show minimal columns in the status table
    Narrow,
    /// Show all columns in the status table
    Wide,
}

derive_fromstr_from_deserialize!(StatusOutput);
