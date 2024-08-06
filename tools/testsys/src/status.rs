use crate::error::{self, Result};
use clap::Parser;
use log::{debug, info};
use serde::Deserialize;
use serde_plain::derive_fromstr_from_deserialize;
use snafu::ResultExt;
use std::collections::HashMap;
use testsys_model::test_manager::{CrdState, CrdType, ResultType, crd_type, crd_state, crd_results, SelectionParams, StatusColumn, TestManager};
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
            Some(StatusOutput::Chart) => {
                let simple_json: String = create_simple_json(crd_vecs);

                #[derive(Deserialize)]
                struct TestResult {
                    cluster: String,
                    variant: String,
                    arch: String,
                    conformance: String,
                    migration: String,
                    smoke: String,
                    karpenter: String,
                    macis: String,
                }

                fn read_json_string(json_str: &str) -> Vec<TestResult> {
                    serde_json::from_str(json_str).expect("Error parsing JSON")
                }

                let test_results = read_json_string(&simple_json);

                fn color_result(result: &str) -> &'static str {
                    match result {
                        "pass" | "passed" | "Passed" => "FwBg",
                        "error" | "fail" | "failed" | "Failed" => "FdBr",
                        "ipv4" | "ipv6" => "FdBy",
                        "skip" | "n/a" | "skipped" | "Skipped" => "FwBs",
                        _ => "",
                    }
                }

                let mut table = Table::new();

                table.add_row(Row::new(vec![
                    Cell::new("Cluster"),
                    Cell::new("Variant"),
                    Cell::new("Arch"),
                    Cell::new("Conformance"),
                    Cell::new("Migration"),
                    Cell::new("Smoke"),
                    Cell::new("Karpenter"),
                    Cell::new("Macis"),
                ]));

                for result in test_results {
                    table.add_row(Row::new(vec![
                        Cell::new(&result.cluster),
                        Cell::new(&result.variant),
                        Cell::new(&result.arch),
                        Cell::new(&result.conformance).style_spec(color_result(&result.conformance)),
                        Cell::new(&result.migration).style_spec(color_result(&result.migration)),
                        Cell::new(&result.smoke).style_spec(color_result(&result.smoke)),
                        Cell::new(&result.karpenter).style_spec(color_result(&result.karpenter)),
                        Cell::new(&result.macis).style_spec(color_result(&result.macis)),
                    ]));
                }

                table.set_format(
                    format::FormatBuilder::new()
                        .column_separator('│')
                        .borders('│')
                        .separators(
                            &[format::LinePosition::Top],
                            format::LineSeparator::new('─', '┬', '┌', '┐'),
                        )
                        .separators(
                            &[format::LinePosition::Top],
                            format::LineSeparator::new('─', '┬', '┌', '┐'),
                        )
                        .separators(
                            &[format::LinePosition::Intern],
                            format::LineSeparator::new('─', '┼', '├', '┤'),
                        )
                        .separators(
                            &[format::LinePosition::Bottom],
                            format::LineSeparator::new('─', '┴', '└', '┘'),
                        )
                        .padding(1, 1)
                        .build(),
                );

                table.printstd();
            }
            Some(StatusOutput::Condensed) => {
                fn condense_crd_data(crd_data_vecs: &Vec<Crd>) -> Vec<Vec<String>> {
                    let full_crd_data: Vec<Vec<String>> = extract_full_crd_data(crd_data_vecs);
                    let mut result_data: Vec<Vec<String>> = vec![vec![], vec![], vec![], vec![], vec![], vec![], vec![], vec![], vec![]];

                    for i in 0..full_crd_data.len() {
                        let curr_arch = &full_crd_data[i][3].clone();
                        let curr_variant = &full_crd_data[i][4].clone();
                        let curr_cluster = &full_crd_data[i][2].clone();
                        if full_crd_data[i][1] != "migration" {
                            add_row(&mut result_data, &full_crd_data, i);
                        } else if i + 4 < full_crd_data.len() && full_crd_data[i + 4][1] == "migration" && (*curr_arch == full_crd_data[i + 4][3]) && (*curr_variant == full_crd_data[i + 4][4]) && (*curr_cluster == full_crd_data[i + 4][2]) {
                            if full_crd_data[i + 4][6] != "" && full_crd_data[i + 4][6].parse::<i32>().unwrap() > 0 && full_crd_data[i + 4][7].parse::<i32>().unwrap() == 0 {
                                let sum_passed: String = (
                                    full_crd_data[i][6].parse::<i32>().unwrap()
                                    + full_crd_data[i + 1][6].parse::<i32>().unwrap()
                                    + full_crd_data[i + 2][6].parse::<i32>().unwrap()
                                    + full_crd_data[i + 3][6].parse::<i32>().unwrap()
                                    + full_crd_data[i + 4][6].parse::<i32>().unwrap()
                                ).to_string();
                                let sum_skipped: String = (
                                    full_crd_data[i][8].parse::<i32>().unwrap()
                                    + full_crd_data[i + 1][8].parse::<i32>().unwrap()
                                    + full_crd_data[i + 2][8].parse::<i32>().unwrap()
                                    + full_crd_data[i + 3][8].parse::<i32>().unwrap()
                                    + full_crd_data[i + 4][8].parse::<i32>().unwrap()
                                ).to_string();

                                add_row_with_sums(&mut result_data, &full_crd_data, i, sum_passed, sum_skipped);
                            } else {
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
                    if full_crd_data[i][0] == "Resource" {
                        result_data[0].push(full_crd_data[i][0].clone());
                        result_data[1].push(full_crd_data[i][1].clone());
                        result_data[2].push(full_crd_data[i][2].clone());
                        result_data[3].push(full_crd_data[i][3].clone());
                        result_data[4].push(full_crd_data[i][4].clone());
                        result_data[5].push(full_crd_data[i][5].clone());
                    } else {
                        result_data[0].push(full_crd_data[i][0].clone());
                        result_data[1].push(full_crd_data[i][1].clone());
                        result_data[2].push(full_crd_data[i][2].clone());
                        result_data[3].push(full_crd_data[i][3].clone());
                        result_data[4].push(full_crd_data[i][4].clone());
                        result_data[5].push(full_crd_data[i][5].clone());
                        result_data[6].push(full_crd_data[i][6].clone());
                        result_data[7].push(full_crd_data[i][7].clone());
                        result_data[8].push(full_crd_data[i][8].clone());
                    }
                }

                fn add_row_with_sums(result_data: &mut Vec<Vec<String>>, full_crd_data: &Vec<Vec<String>>, i: usize, sum_passed: String, sum_skipped: String) {
                    result_data[0].push(full_crd_data[i][0].clone());
                    result_data[1].push(full_crd_data[i][1].clone());
                    result_data[2].push(full_crd_data[i][2].clone());
                    result_data[3].push(full_crd_data[i][3].clone());
                    result_data[4].push(full_crd_data[i][4].clone());
                    result_data[5].push(full_crd_data[i][5].clone());
                    result_data[6].push(sum_passed);
                    result_data[7].push(full_crd_data[i][7].clone());
                    result_data[8].push(sum_skipped);
                }

                fn extract_full_crd_data(crd_vecs: &Vec<Crd>) -> Vec<Vec<String>> {
                    let mut full_data: Vec<Vec<String>> = Vec::new();
                    for crd in crd_vecs {
                        full_data.push(
                            extract_crd_data(crd)
                                .clone()
                                .into_iter()
                                .flat_map(|v| {
                                    if v.is_empty() {
                                        vec![String::from("")]
                                    } else {
                                        v
                                    }
                                })
                                .collect()
                        );
                    }
                    println!("{:?}", full_data);
                    full_data
                }


                /*
                status.new_column("CRD-TYPE", |crd| {
                    let test = vec!["hi".to_string()];
                    test
                });
                */

                let test_condense = condense_crd_data(crd_vecs)[0].clone();


                status.new_column("CRD-TYPE", |crd| {
                    //condense_full_crd_data(crd_vecs)[0];
                    test_condense
                });
            }
            Some(StatusOutput::Narrow) => {
                status.add_column(StatusColumn::name());
                status.add_column(StatusColumn::crd_type());
                status.add_column(StatusColumn::state());
                status.add_column(StatusColumn::passed());
                status.add_column(StatusColumn::failed());
                status.add_column(StatusColumn::skipped());
            }
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
    /// Show condensed output in the simplified status table
    Condensed,
    /// Display a chart of the testsys results
    Chart,
    /// Show minimal columns in the status table
    Narrow,
    /// Show all columns in the status table
    Wide,
}

derive_fromstr_from_deserialize!(StatusOutput);
