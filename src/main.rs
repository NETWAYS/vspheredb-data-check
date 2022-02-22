use clap::{Args, Parser, Subcommand};
use icingaplugin_rs::{check::{CheckResult, Metric, PerfData}, utils::evaluate};
use sqlx::{Connection, MySqlConnection, mysql::MySqlRow, Row};
use std::convert::TryInto;
use std::ops::Deref;
use std::process::exit;

/// A check plugin for retrieving performance data of vSphere hosts collected by Icingaweb2's vSphereDB modul.
///
/// The vSphereDB module collects lots of useful information and performance data from the vCenters
/// it queries, but without proper alert management on the vCenters' side, this information is
/// rendered merily cosmetical and not useful for alerting.
/// This plugin allows to query the
/// collected data via vSphereDB's database tables and enables Icinga2 admins to trigger alerts on
/// their side of the monitoring.
#[derive(Parser)]
#[clap(author, version, about)]
struct App {

    /// check to execute
    #[clap(subcommand)]
    check: Checks,

    /// machine to be queried for
    #[clap(short, long)]
    machine: String,
}

#[derive(Args)]
struct Settings {
    /// database host to connect to
    #[clap(short='H', long, default_value="localhost")]
    host: String,


    /// database port to connect to
    #[clap(short, long, default_value="3306")]
    port: u16,

    /// database name
    #[clap(short, long, default_value="vspheredb")]
    database: String,

    /// database user
    #[clap(short, long, default_value="vspheredb")]
    user: String,

    /// database password
    #[clap(short='P', long, default_value="vspheredb")]
    password: String,

}


#[derive(Subcommand)]
enum Checks {
    /// checks CPU usage
    Cpu {
        #[clap(flatten)]
        settings: Settings,

        /// warning threshold as integer (80%)
        #[clap(short, long)]
        warning: Option<u32>,

        /// critical threshold as integer (90%)
        #[clap(short, long)]
        critical: Option<u32>,
    },

    /// checks memory usage
    Memory {
        #[clap(flatten)]
        settings: Settings,

        /// warning threshold as integer (80%)
        #[clap(short, long)]
        warning: Option<u32>,

        /// critical threshold as integer (90%)
        #[clap(short, long)]
        critical: Option<u32>,
    },

    /// checks temperature
    Temperature {
        #[clap(flatten)]
        settings: Settings,

        /// warning threshold as integer (50°C)
        #[clap(short, long)]
        warning: Option<u32>,

        /// critical threshold as integer (60°C)
        #[clap(short, long)]
        critical: Option<u32>,
    },

    /// checks attached NICs
    Nic {
        #[clap(flatten)]
        settings: Settings,

        /// warning threshold as integer (1)
        #[clap(short, long)]
        warning: Option<u32>,

        /// critical threshold as integer (0)
        #[clap(short, long)]
        critical: Option<u32>,
    },

    /// checks attached HBAs
    Hba {
        #[clap(flatten)]
        settings: Settings,

        /// warning threshold as integer (1)
        #[clap(short, long)]
        warning: Option<u32>,

        /// critical threshold as integer (0)
        #[clap(short, long)]
        critical: Option<u32>,
    },
    Datastore {
        #[clap(flatten)]
        settings: Settings,

        /// optional specific Datastore
        #[clap(short, long)]
        store: Option<String>,
    },
}

impl Deref for Checks {
    type Target = Settings;
    fn deref(&self) -> &Settings {
        match self {
            Checks::Cpu{settings, ..} => settings,
            Checks::Memory{settings, ..} => settings,
            Checks::Temperature{settings, ..} => settings,
            Checks::Nic{settings, ..} => settings,
            Checks::Hba{settings, ..} => settings,
            Checks::Datastore{settings, ..} => settings,
        }
    }
}


impl Checks {
    /// Builds and returns a query for a given machine and a given check type
    fn build_query(&self, machine: &String) -> String {
        let mut query = String::new();
        match self {
            Checks::Cpu{..} => {
                query.push_str("SELECT hqs.overall_cpu_usage, 
                               hs.hardware_cpu_mhz, 
                               hs.hardware_cpu_cores 
                               FROM host_quick_stats hqs 
                               INNER JOIN host_system hs 
                               ON hqs.uuid = hs.uuid 
                               WHERE hs.host_name LIKE \"");
                query.push_str(machine);
                query.push_str("\";");
                return query;
            },
            Checks::Memory{..} => {
                query.push_str("SELECT hqs.overall_memory_usage_mb, 
                               hs.hardware_memory_size_mb 
                               FROM host_quick_stats hqs 
                               INNER JOIN host_system hs 
                               ON hqs.uuid = hs.uuid 
                               WHERE hs.host_name LIKE \"");
                query.push_str(machine);
                query.push_str("\";");
                return query;
            },
            Checks::Temperature{..} => {
                query.push_str("SELECT se.current_reading 
                               FROM host_sensor se 
                               INNER JOIN host_system hs 
                               ON se.host_uuid = hs.uuid 
                               WHERE hs.host_name LIKE \"");
                query.push_str(machine);
                query.push_str("\" AND se.name LIKE \"System Board 1 Inlet Temp\"");
                query.push_str(";");
                return query;
            },
            Checks::Nic{..} => {
                query.push_str("SELECT hardware_num_nic 
                               FROM host_system 
                               WHERE host_system.host_name LIKE \"");
                query.push_str(machine);
                query.push_str("\";");
                return query;
            },
            Checks::Hba{..} => {
                query.push_str("SELECT hardware_num_hba 
                               FROM host_system 
                               WHERE host_system.host_name LIKE \"");
                query.push_str(machine);
                query.push_str("\";");
                return query;
            },
            Checks::Datastore{store, ..} => {
                query.push_str("SELECT o.object_name, ds.maintenance_mode, ds.is_accessible, ds.capacity, ds.free_space 
                               FROM datastore ds 
                               INNER JOIN vcenter vc 
                               ON ds.vcenter_uuid = vc.instance_uuid ");
                if let Some(s) = store {
                    query.push_str("INNER JOIN object o 
                                   ON ds.uuid = o.uuid 
                                   WHERE o.object_name LIKE \"");
                    query.push_str(s);
                    query.push_str("\" AND ");
                } else {
                    query.push_str("WHERE ")
                }
                query.push_str("vc.name LIKE \"");
                query.push_str(machine);
                query.push_str("\";");
                return query;
            },
        }
    }

    fn process_results(self, row: MySqlRow) -> Result<(), sqlx::Error> {
        let mut metrics: Vec<Metric> = Vec::new();
        let status_msg: String;
        let warn: u32;
        let crit: u32;
        match self {
            Checks::Cpu{warning, critical, ..} => {
                warn = warning.unwrap_or(80);
                crit = critical.unwrap_or(90);
                let value0 = row.get::<u32, usize>(0);
                let value1 = row.get::<u32, usize>(1);
                let value2 = row.get::<u32, usize>(2);
                let value: u32 = (value0 * 100 / (value1 * value2)).into();
                metrics.push(Metric::new(String::from("usage"), value0.to_string()));
                metrics.push(Metric::new(String::from("usage_percent"), value.to_string() + "%")
                             .warning(warn.to_string() + "%")
                             .critical(crit.to_string() + "%"));
                metrics.push(Metric::new(String::from("mhz"), value1.to_string()));
                metrics.push(Metric::new(String::from("cores"), value2.to_string()));

                status_msg = format!("Total CPU usage is {}GHz ({}%)", value0 / 1024, value);
                let check_result = evaluate(value, warn, crit);
                exit(
                    check_result.set_info(status_msg)
                    .set_perf_data(PerfData::from_metrics(metrics))
                    .promote())
            },
            Checks::Memory{warning, critical, ..} => {
                warn = warning.unwrap_or(80); 
                crit = critical.unwrap_or(90); 
                let value0 = row.get::<u32, usize>(0);
                let value1 = row.get::<u32, usize>(1);
                let value: u32 = (value0 * 100 / value1).into();
                metrics.push(Metric::new(String::from("usage"), value0.to_string() + "MB"));
                metrics.push(Metric::new(String::from("usage_percent"), value.to_string() + "%")
                             .warning(warn.to_string() + "%")
                             .critical(crit.to_string() + "%"));
                metrics.push(Metric::new(String::from("capacity"), value1.to_string() + "MB"));

                status_msg = format!("Total memory usage is {}GB ({}%)", value0 / 1024, value);
                let check_result = evaluate(value, warn, crit);
                exit(
                    check_result.set_info(status_msg)
                    .set_perf_data(PerfData::from_metrics(metrics))
                    .promote())
            },
            Checks::Temperature{warning, critical, ..} => {
                warn = warning.unwrap_or(50);
                crit = critical.unwrap_or(60);
                let value: u32 = (row.get::<i32, usize>(0) / 100).try_into().unwrap();
                metrics.push(Metric::new(String::from("temp"), value.to_string() + "C")
                             .warning(warn.to_string() + "C")
                             .critical(crit.to_string() + "C"));

                status_msg = format!("Temperature is {}°C", value);
                let check_result = evaluate(value, warn, crit);
                exit(
                    check_result.set_info(status_msg)
                    .set_perf_data(PerfData::from_metrics(metrics))
                    .promote())
            },
            Checks::Nic{warning, critical, ..} => {
                warn = warning.unwrap_or(1);
                crit = critical.unwrap_or(0);
                let value: u8 = row.get(0);
                metrics.push(Metric::new(String::from("nics"), value.to_string())
                             .warning(warn.to_string())
                             .critical(crit.to_string()));

                let check_result = evaluate(value, warn, crit);
                exit(
                    check_result.set_info(format!("Number of NICs: {}", value.to_string()))
                    .set_perf_data(PerfData::from_metrics(metrics))
                    .promote())
            },
            Checks::Hba{warning, critical, ..} => {
                warn = warning.unwrap_or(1);
                crit = critical.unwrap_or(0);
                let value: u8 = row.get(0);
                metrics.push(Metric::new(String::from("hbas"), value.to_string())
                             .warning(warn.to_string())
                             .critical(crit.to_string()));

                let check_result = evaluate(value, warn, crit);
                exit(
                    check_result.set_info(format!("Number of HBAs: {}", value.to_string()))
                    .set_perf_data(PerfData::from_metrics(metrics))
                    .promote())
            },
            Checks::Datastore{store, ..} => Ok(()),
        }
    }
}

#[async_std::main]
async fn main() -> Result<(), sqlx::Error> {
    let args = App::parse();
    let query = args.check.build_query(&args.machine);
  
    let mut conn: MySqlConnection; 
    let mut address = String::from("mysql://");
    address += &args.check.user;
    address.push_str(":");
    address += &args.check.password;
    address.push_str("@");
    address += &args.check.host;
    address.push_str(":");
    address += &args.check.port.to_string();
    address.push_str("/");
    address += &args.check.database;
    match MySqlConnection::connect(&address).await {
        Ok(c) => {
            conn = c;
            let result = sqlx::query(&query).fetch_one(&mut conn).await;
            if let Ok(r) = result {
                    args.check.process_results(r)?;
            } else {
                exit(
                    CheckResult::from(1)
                    .set_info(format!("Query returned no results"))
                    .promote());
            }
        },
        Err(e) => 
            exit(
                CheckResult::from(2)
                .set_info(format!("Could not connect to database: {}", e))
                .promote())
    };
    Ok(())
}
