use crate::error::{self, Result};
use clap::Parser;
use log::{debug, info};
use serde::Deserialize;
use serde_plain::derive_fromstr_from_deserialize;
use snafu::ResultExt;
use testsys_model::test_manager::{
    CrdState, CrdType, ResultType, crd_state, crd_results, SelectionParams, StatusColumn, TestManager,
};
use testsys_model::Crd;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::time::sleep;

/// Check the status of testsys objects.
#[derive(Debug, Parser)]
pub(crate) struct Status {
    /// Configure the output of the command (json, narrow, wide).
    #[arg(long, short = 'o')]
    output: Option<StatusOutput>,

    /// Focus status on a particular arch
    #[arg(long)]
    arch: Option<String>,

    /// Focus status on a particular variant
    #[arg(long)]
    variant: Option<String>,

    /// Provide an interval (in seconds) to refresh the status output
    #[arg(long, short = 'r')]
    refresh: Option<u64>,

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
        if let Some(refresh) = self.refresh {
            loop {
                clear_screen();
                self.run_status(&client).await?;
                sleep(Duration::from_secs(refresh)).await;
            }
        } else {
            // Call the status as normal.
            self.run_status(&client).await?;
        }
        Ok(())
    }

    pub async fn run_status(&self, client: &TestManager) -> Result<()> {
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
        if let Some(ref arch) = self.arch {
            labels.push(format!("testsys/arch={}", arch));
        }
        if let Some(ref variant) = self.variant {
            labels.push(format!("testsys/variant={}", variant));
        }

        let mut status = client
            .status(&SelectionParams {
                labels: Some(labels.join(",")),
                state: state.clone(),
                crd_type: crd_type.clone(),
                ..Default::default()
            })
            .await?;

        // Extract data from one CRD
        fn extract_crd_data(crd: &Crd) -> Vec<Vec<String>> {
            let mut crd_data = Vec::new();

            // Extract crd type data
            let crd_type_data: Vec<String> = testsys_model::test_manager::crd_type(crd);
            crd_data.push(crd_type_data);

            // Extract test type data
            let test_type_data = crd.labels()
                .get("testsys/type")
                .cloned()
                .into_iter()
                .collect();
            crd_data.push(test_type_data);

            // Extract test cluster data - index 2
            let test_type_data = crd.labels()
                .get("testsys/cluster")
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

            // Extract current status data
            let status_data: Vec<String> = crd_state(crd);
            crd_data.push(status_data);

            // Extract passed, failed, and skipped data
            let passed_data = crd_results(crd, ResultType::Passed);
            let failed_data = crd_results(crd, ResultType::Failed);
            let skipped_data = crd_results(crd, ResultType::Skipped);
            crd_data.push(passed_data);
            crd_data.push(failed_data);
            crd_data.push(skipped_data);

            crd_data
        }

        let crd_vecs = status.use_crds();

        fn create_simple_json(crd_vec: &Vec<Crd>) -> String {
            let mut result: Vec<BTreeMap<String, String>> = Vec::new();
            let mut variant_data_map: BTreeMap<(String, String, String), BTreeMap<String, String>> = BTreeMap::new();

            for crd in crd_vec.clone() {
                let curr_crd_data = extract_crd_data(&crd).clone();
                if curr_crd_data[0][0] == "Test" {
                    let variant = curr_crd_data[4][0].clone();
                    let arch = curr_crd_data[3][0].clone();
                    let test_type = curr_crd_data[1][0].clone();
                    let status = curr_crd_data[5][0].clone();
                    let cluster = curr_crd_data[2][0].clone();

                    let key = (variant.clone(), arch.clone(), cluster.clone());
                    if !variant_data_map.contains_key(&key) {
                        let mut variant_data: BTreeMap<String, String> = BTreeMap::new();
                        variant_data.insert("variant".to_string(), variant.clone());
                        variant_data.insert("arch".to_string(), arch.clone());
                        variant_data.insert("cluster".to_string(), cluster.clone());
                        variant_data.insert("conformance".to_string(), "n/a".to_string());
                        variant_data.insert("migration".to_string(), "n/a".to_string());
                        variant_data.insert("smoke".to_string(), "n/a".to_string());
                        variant_data.insert("karpenter".to_string(), "n/a".to_string());
                        variant_data.insert("macis".to_string(), "n/a".to_string());
                        variant_data_map.insert(key.clone(), variant_data);
                    }

                    let variant_data = variant_data_map.get_mut(&key).unwrap();
                    if test_type == "conformance" {
                        variant_data.insert("conformance".to_string(), status);
                    } else if test_type == "migration" {
                        variant_data.insert("migration".to_string(), status.clone());
                        if status == "waiting" || status == "error" {
                            variant_data.insert("migration".to_string(), "failed".to_string());
                        }
                    } else if test_type == "smoke" {
                        variant_data.insert("smoke".to_string(), status);
                    } else if test_type == "karpenter" {
                        variant_data.insert("karpenter".to_string(), status);
                    } else if test_type == "macis" {
                        variant_data.insert("macis".to_string(), status);
                    } else {
                        variant_data.insert(test_type, status);
                    }
                }
            }

            for (_, variant_data) in variant_data_map {
                result.push(variant_data);
            }

            let final_result = json!(result);
            let pretty_result: String = serde_json::to_string_pretty(&final_result).unwrap();
            pretty_result
        }


        status.add_column(StatusColumn::name());
        status.add_column(StatusColumn::crd_type());
        status.add_column(StatusColumn::state());
        status.add_column(StatusColumn::passed());
        status.add_column(StatusColumn::failed());
        status.add_column(StatusColumn::skipped());

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
            Some(StatusOutput::SimpleJson) => {
                println!("{}", create_simple_json(crd_vecs));
            }
            Some(StatusOutput::Narrow) => (),
            None => {
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

fn clear_screen() {
    print!("\x1B[2J\x1B[1;1H");
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
enum StatusOutput {
    /// Output the status in json
    Json,
    /// Output the status in a "simple" json format
    SimpleJson,
    /// Show minimal columns in the status table
    Narrow,
    /// Show all columns in the status table
    Wide,
}

derive_fromstr_from_deserialize!(StatusOutput);