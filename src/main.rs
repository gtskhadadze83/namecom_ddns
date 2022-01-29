//! Dynamic DNS with the Name.com API.
//! Run periodically to set DNS records to the local IP.
//
//  Copyright (C) 2021 Zhang Maiyun <myzhang1029@hotmail.com>
//
//  This file is part of DNS updater.
//
//  DNS updater is free software: you can redistribute it and/or modify
//  it under the terms of the GNU Affero General Public License as published by
//  the Free Software Foundation, either version 3 of the License, or
//  (at your option) any later version.
//
//  DNS updater is distributed in the hope that it will be useful,
//  but WITHOUT ANY WARRANTY; without even the implied warranty of
//  MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
//  GNU Affero General Public License for more details.
//
//  You should have received a copy of the GNU Affero General Public License
//  along with DNS updater.  If not, see <https://www.gnu.org/licenses/>.
//

#![warn(
    clippy::pedantic,
    missing_docs,
    missing_debug_implementations,
    missing_copy_implementations,
    trivial_casts,
    trivial_numeric_casts,
    unused_extern_crates,
    unused_import_braces,
    unused_qualifications,
    variant_size_differences
)]

mod api;
mod config;

use clap::{crate_version, App, Arg};
use getip::{get_ip, IpScope, IpType};
use log::{debug, error, info};
use simplelog::{ColorChoice, ConfigBuilder, LevelFilter, TermLogger, TerminalMode};
use std::collections::HashMap;
use std::net::IpAddr;
use std::process::exit;
use std::str::FromStr;
use std::sync::Arc;
use tokio::{join, sync::RwLock, time};

#[tokio::main]
async fn main() {
    let matches = App::new("Name.com DDNS")
        .version(crate_version!())
        .author("Zhang Maiyun <myzhang1029@hotmail.com")
        .about("Query IP addresses and update DNS records with Name.com API")
        .arg(
            Arg::new("config")
                .short('f')
                .long("config-file")
                .help("Set a custom config file")
                .takes_value(true)
                .default_value("namecom_ddns.toml"),
        )
        .arg(
            Arg::new("oneshot")
                .short('s')
                .long("oneshot")
                .help("Only check and update once"),
        )
        .arg(
            Arg::new("loglevel")
                .short('l')
                .long("log-level")
                .help("Set log level")
                .takes_value(true)
                .possible_values(&["off", "error", "warn", "info", "debug", "trace"])
                .default_value("info"),
        )
        .get_matches();

    // Initialize logger
    let log_format = ConfigBuilder::new()
        .set_time_to_local(true)
        .set_time_format_str("[%Y-%m-%d %H:%M:%S]")
        .build();

    // There unwrap()s are guaranteed to succeed by clap
    if let Err(e) = TermLogger::init(
        LevelFilter::from_str(matches.value_of("loglevel").unwrap()).unwrap(),
        log_format,
        TerminalMode::Mixed,
        ColorChoice::Auto,
    ) {
        panic!("Cannot create logger: {:?}", e);
    }

    let configuration = config::NameComDdnsConfig::from_file(matches.value_of("config").unwrap())
        .expect("Cannot open configuration");

    // Check and update the DNS according to the config
    let mut interval = time::interval(time::Duration::from_secs(configuration.core.interval * 60));
    debug!("Configuration: {:?}", configuration);
    let url = configuration.core.url;
    // Create a API client
    let client = api::NameComDnsApi::create(
        &configuration.core.username,
        &configuration.core.key,
        &url,
        configuration.core.timeout,
    )
    .unwrap();
    let app = DdnsApp::new(&configuration.records, &client);
    if matches.is_present("oneshot") {
        exit(if app.update_once().await { 0 } else { 1 });
    } else {
        app.updater_loop(&mut interval).await;
    }
}

/// Struct containing the application's data
struct DdnsApp<'a> {
    client: &'a api::NameComDnsApi,
    records: &'a [config::NameComConfigRecord],
    id_cache: Arc<RwLock<HashMap<config::NameComConfigRecord, i32>>>,
}

impl<'a> DdnsApp<'a> {
    /// Create a new App.
    fn new(records: &'a [config::NameComConfigRecord], client: &'a api::NameComDnsApi) -> Self {
        Self {
            client,
            records,
            id_cache: Arc::new(RwLock::new(HashMap::with_capacity(records.len()))),
        }
    }

    /// Update every record specified.
    async fn update_once(&self) -> bool {
        futures::future::join_all(
            self.records
                .iter()
                .map(|item| self.update_single_item(item)),
        )
        .await
        .iter()
        .all(|a| *a)
    }

    /// Main loop for updating every entry.
    async fn updater_loop(&self, interval: &mut time::Interval) {
        loop {
            interval.tick().await;
            info!("Checking and updating addresses");
            self.update_once().await;
            info!("Finished checking and updating addresses");
        }
    }

    /// Get the id of a records, and cache it.
    async fn get_id(&self, item: &config::NameComConfigRecord) -> reqwest::Result<Option<i32>> {
        let id = {
            // Make sure the read copy goes out of scope
            let cache = self.id_cache.read().await;
            // Hack to convert Option<&i32> to Option<i32>
            (|| Some(*cache.get(item)?))()
        };
        // Check if the id still points to the same record skipped
        Ok(if matches!(id, None) {
            let matches = self
                .client
                .search_records(&item.zone, item.rec_type, Some(&item.host))
                .await?;
            if matches.is_empty() {
                None
            } else {
                let mut cache = self.id_cache.write().await;
                cache.insert(item.clone(), matches[0]);
                Some(matches[0])
            }
        } else {
            id
        })
    }

    /// Check and update a single record item.
    async fn update_single_item(&self, item: &config::NameComConfigRecord) -> bool {
        let (answer, old) = join!(self.get_ip_by_item(item), self.get_id(item));

        if let Ok(addr) = answer {
            info!("Received answer for {} is {}", item.host, addr);
            let new_record = api::NameComNewRecord {
                host: Some(item.host.clone()),
                rec_type: item.rec_type,
                answer: addr.to_string(),
                ttl: item.ttl,
                priority: None,
            };
            match old {
                Ok(maybe_id) => {
                    // Succeeded to receive ID
                    let update_result = if let Some(id) = maybe_id {
                        // Update existing record
                        self.client.update_record(&item.zone, id, &new_record).await
                    } else {
                        // No existing record found, put a new one
                        self.client.create_record(&item.zone, &new_record).await
                    };
                    if let Err(error) = update_result {
                        error!(
                            "Failed to update the record for {} via API: {:?}",
                            item.host, error
                        );
                        false
                    } else {
                        true
                    }
                }
                Err(error) => {
                    error!(
                        "Failed to query records of {} via API: {:?}",
                        item.host, error
                    );
                    false
                }
            }
        } else {
            error!(
                "Failed to receive the IP for {}: {:?}",
                item.host,
                answer.unwrap_err()
            );
            false
        }
    }

    /// Wrapper to get IP addresses with `getip`.
    async fn get_ip_by_item(
        &self,
        item: &config::NameComConfigRecord,
    ) -> Result<IpAddr, getip::Error> {
        match (item.rec_type, item.method) {
            (api::RecordType::A, config::NameComConfigMethod::Global) => {
                get_ip(IpType::Ipv4, IpScope::Global, None).await
            }
            (api::RecordType::Aaaa, config::NameComConfigMethod::Global) => {
                get_ip(IpType::Ipv6, IpScope::Global, None).await
            }
            (api::RecordType::A, config::NameComConfigMethod::Local) => {
                get_ip(IpType::Ipv4, IpScope::Local, Some(&item.interface)).await
            }
            (api::RecordType::Aaaa, config::NameComConfigMethod::Local) => {
                get_ip(IpType::Ipv6, IpScope::Local, Some(&item.interface)).await
            }
            _ => panic!(
                "Record type {} is not one of \"A\" and \"AAAA\"",
                item.rec_type
            ),
        }
    }
}
