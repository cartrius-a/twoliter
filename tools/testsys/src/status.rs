use crate::error::{self, Result};
use clap::Parser;
use log::{debug, info};
use serde::Deserialize;
use serde_json::{json, Value};
use serde_plain::derive_fromstr_from_deserialize;
use snafu::ResultExt;
use std::collections::HashMap;
use testsys_model::test_manager::{CrdState, CrdType, ResultType, crd_type, crd_results, SelectionParams, StatusColumn, TestManager};
use testsys_model::Crd; // Unsure if this import is needed


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

        fn extract_crd_data(crd: &Crd) -> Vec<Vec<String>> {
            let mut crd_data = Vec::new();
        
            let crd_type_data: Vec<String> = testsys_model::test_manager::crd_type(crd);
            crd_data.push(crd_type_data);
        
            // Extract test type data
            let test_type_data = crd.labels()
                .get("testsys/type")
                .cloned()
                .into_iter()
                .collect();
            crd_data.push(test_type_data);
        
            // Extract arch data
            let arch_data = crd.labels()
                .get("testsys/arch")
                .cloned()
                .into_iter()
                .collect();
            crd_data.push(arch_data);
        
            // Extract variant data
            let variant_data = crd.labels()
                .get("testsys/variant")
                .cloned()
                .into_iter()
                .collect();
            crd_data.push(variant_data);
        
            // Extract passed, failed, and skipped data
            let passed_data = crd_results(crd, ResultType::Passed);
            let failed_data = crd_results(crd, ResultType::Failed);
            let skipped_data = crd_results(crd, ResultType::Skipped);
            crd_data.push(passed_data);
            crd_data.push(failed_data);
            crd_data.push(skipped_data);
        
            let full_crd_data = crd_data.clone();
        
            // result_data should also be a vector of vectors in the same format of crd_data but only with the "rows" that were are adding
            // The vectors within should resprwsent the colunns so when we fill result_data, we should be pushing to a specific index within result_data
            let mut result_data: Vec<Vec<String>> = vec![vec![], vec![], vec![], vec![], vec![], vec![], vec![]];
        
            for i in 0..full_crd_data[1].len() {
                let curr_variant = &full_crd_data[3][i].clone();
                if full_crd_data[1][i] != "migration" {
                    add_row(&mut result_data, &full_crd_data, i);
                } else if i + 4 < full_crd_data[1].len() && full_crd_data[1][i + 4] == "migration" && (*curr_variant == full_crd_data[3][i + 4]) {
                    if crd_data[3][i + 4].parse::<i32>().unwrap() > 0 && crd_data[4][i + 4].parse::<i32>().unwrap() == 0 { //If 5th migration passed - A Pass should include Passing > 0 and Failed == 0
                        let sum_passed: String = (
                            full_crd_data[4][i].parse::<i32>().unwrap()
                            + full_crd_data[4][i + 1].parse::<i32>().unwrap()
                            + full_crd_data[4][i + 2].parse::<i32>().unwrap()
                            + full_crd_data[4][i + 3].parse::<i32>().unwrap()
                            + full_crd_data[4][i + 4].parse::<i32>().unwrap()
                        ).to_string();
                        let sum_skipped: String = (
                            full_crd_data[6][i].parse::<i32>().unwrap()
                            + full_crd_data[6][i + 1].parse::<i32>().unwrap()
                            + full_crd_data[6][i + 2].parse::<i32>().unwrap()
                            + full_crd_data[6][i + 3].parse::<i32>().unwrap()
                            + full_crd_data[6][i + 4].parse::<i32>().unwrap()
                        ).to_string();
        
                        add_row_with_sums(&mut result_data, &full_crd_data, i, sum_passed, sum_skipped);
                    } else { //If 5th migration failed
                        add_row(&mut result_data, &full_crd_data, i);
                        add_row(&mut result_data, &full_crd_data, i + 1);
                        add_row(&mut result_data, &full_crd_data, i + 2);
                        add_row(&mut result_data, &full_crd_data, i + 3);
                        add_row(&mut result_data, &full_crd_data, i + 4);
                    }
                } else {
                    continue;
                }
            }
            result_data
        }
        
        fn add_row(result_data: &mut Vec<Vec<String>>, full_crd_data: &Vec<Vec<String>>, i: usize) {
            if full_crd_data[0][i] == "Resource" {
                result_data[0].push(full_crd_data[0][i].clone()); // Displayed as TYPE - crd_type
                result_data[1].push(full_crd_data[1][i].clone()); // Displayed as TEST-TYPE -
                result_data[2].push(full_crd_data[2][i].clone()); // Displayed as ARCH
                result_data[3].push(full_crd_data[3][i].clone()); // Displayed as VARIANT
            } else {
                result_data[0].push(full_crd_data[0][i].clone()); // Displayed as TYPE - crd_type
                result_data[1].push(full_crd_data[1][i].clone()); // Displayed as TEST-TYPE -
                result_data[2].push(full_crd_data[2][i].clone()); // Displayed as ARCH
                result_data[3].push(full_crd_data[3][i].clone()); // Displayed as VARIANT
                result_data[4].push(full_crd_data[4][i].clone()); // Displayed as PASSED
                result_data[5].push(full_crd_data[5][i].clone()); // Displayed as FAILED
                result_data[6].push(full_crd_data[6][i].clone()); // Displayed as SKIPPED
            }
        }
        
        fn add_row_with_sums(result_data: &mut Vec<Vec<String>>, full_crd_data: &Vec<Vec<String>>, i: usize, sum_passed: String, sum_skipped: String) {
            result_data[0].push(full_crd_data[0][i].clone()); // Displayed as TYPE - crd_type
            result_data[1].push(full_crd_data[1][i].clone()); // Displayed as TEST-TYPE -
            result_data[2].push(full_crd_data[2][i].clone()); // Displayed as ARCH
            result_data[3].push(full_crd_data[3][i].clone()); // Displayed as VARIANT
            result_data[4].push(sum_passed); // Displayed as PASSED
            result_data[5].push(full_crd_data[5][i].clone()); // Displayed as FAILED
            result_data[6].push(sum_skipped); // Displayed as SKIPPED
        }

        let mut default_status = || {
            status.add_column(StatusColumn::name());
            status.add_column(StatusColumn::crd_type());
            status.add_column(StatusColumn::state());
            status.add_column(StatusColumn::passed());
            status.add_column(StatusColumn::failed());
            status.add_column(StatusColumn::skipped());
        };

        match self.output {
            Some(StatusOutput::Json) => {
                info!(
                    "{}",
                    serde_json::to_string_pretty(&status).context(error::SerdeJsonSnafu {
                        what: "Could not create string from status."
                    })?
                );
                return Ok(());
            }

            Some(StatusOutput::Condensed) => {
                status.new_column("CRD-TYPE", |crd| {
                    extract_crd_data(crd)[0].clone()
                });
                status.new_column("TEST-TYPE", |crd| {
                    extract_crd_data(crd)[1].clone()
                });
                status.new_column("ARCH", |crd| {
                    extract_crd_data(crd)[2].clone()
                });
                status.new_column("VARIANT", |crd| {
                    extract_crd_data(crd)[3].clone()
                });
                status.new_column("PASSED", |crd| {
                    extract_crd_data(crd)[4].clone()
                });
                status.new_column("FAILED", |crd| {
                    extract_crd_data(crd)[5].clone()
                });
                status.new_column("SKIPPED", |crd| {
                    extract_crd_data(crd)[6].clone()
                });
                status.new_column("VARIANT", |crd| {
                    crd.labels()
                        .get("testsys/variant")
                        .cloned()
                        .into_iter()
                        .collect()
                });
                status.add_column(StatusColumn::passed());
                status.add_column(StatusColumn::failed());
            }
            Some(StatusOutput::Narrow) => {
                default_status();
            },
            None => {
                default_status();
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
                default_status();
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
    /// Show condensed and simplified status table
    Condensed,
    /// Show minimal columns in the status table
    Narrow,
    /// Show all columns in the status table
    Wide,
}

derive_fromstr_from_deserialize!(StatusOutput);
