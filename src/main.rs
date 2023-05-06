use futures::stream::{unfold, StreamExt};
use log::{debug, error, info, warn};
use maxminddb::geoip2;
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io::{self, BufRead};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;
use tokio::net::UdpSocket;
use tokio::time::{interval_at, Duration, Instant, Interval, MissedTickBehavior};
use tokio::{pin, select};

const ERR_TIMEZONE_NOT_FOUND: &[u8] = "ERROR Timezone Not Found".as_bytes();
const ERR_GEOIP_LOOKUP_FAILED: &[u8] = "ERROR GeoIP Lookup Failed".as_bytes();
const ERR_COUNTRY_NOT_FOUND: &[u8] = "ERROR Country Not Found".as_bytes();
const ERR_COUNTRY_SPANS_MULTIPLE_TIMEZONES: &[u8] =
    "ERROR Country Spans Multiple Timezones".as_bytes();

const MAX_REQUEST_SIZE: usize = 512;
const SECONDS_PER_DAY: u64 = 86400;

const UPDATE_TZDATA_SH_PATH: &str = "./update_tzdata.sh";
const UPDATE_MMDB_SH_PATH: &str = "./update_mmdb.sh";
const POSIXINFO_FILE: &str = "posixinfo";
const ZONETAB_FILE: &str = "zone1970.tab";
const MMDB_CITY_FILE: &str = "GeoLite2-City.mmdb";

macro_rules! sh {
    ($path:expr, $($arg:expr),*) => {
        async {
            use async_process::Command;
            match Command::new("sh").arg($path)$(.arg($arg))*.status().await? {
                status if !status.success() => Err(format!("{}", status).into()),
                _ => Ok(()),
            }
        }
    };
}

#[derive(Debug)]
struct Timezone {
    olson: String,
    posix: String,
}

#[derive(Debug)]
struct TimezoneDb {
    timezones: Vec<Timezone>,
    olson_map: HashMap<String, usize>,
    country_map: HashMap<String, Vec<usize>>,
}

impl TimezoneDb {
    async fn update(config: &Config) -> Result<(), Box<dyn Error>> {
        info!("Updating timezone database...");
        sh!(UPDATE_TZDATA_SH_PATH, &config.data_dir).await
    }

    fn load(config: &Config) -> Result<Self, Box<dyn Error>> {
        let mut db = TimezoneDb {
            timezones: Vec::new(),
            olson_map: HashMap::new(),
            country_map: HashMap::new(),
        };

        // Read timezones
        let posixinfo = config.data_path(POSIXINFO_FILE);
        info!("Loading timezones from {}", posixinfo.display());
        for line in read_file_lines(posixinfo)? {
            let [olson, posix] = line.split_whitespace().collect::<Vec<_>>()[..] else {
                warn!("posixinfo entry is improperly formatted, skipping: {}", line);
                continue;
            };
            db.add_timezone(olson, posix)?;
        }
        info!("{} timezones loaded", db.timezones.len());

        // Read countries
        let zonetab = config.data_path(ZONETAB_FILE);
        info!("Loading countries from {}", zonetab.display());
        for line in read_file_lines(zonetab)? {
            if line.starts_with('#') {
                continue;
            }
            let [countries, _, olson, ..] = line.split('\t').collect::<Vec<_>>()[..] else {
                warn!("zone1970.tab entry is improperly formatted, skipping: {}", line);
                continue;
            };
            for country in countries.split(',') {
                db.add_country_timezone(country, olson)?;
            }
        }
        info!("{} countries loaded", db.country_map.len());

        // Custom timezone rules, currently copied as-is from eztime

        if let Some(gb) = db.country_map.get("GB") {
            // https://github.com/ropg/ezTime/blob/7b3c8aa020be818ac149e0762543ac5e81ccfabe/server/server#L112
            debug!("Aliasing 'UK' to 'GB'");
            db.country_map.insert("UK".into(), gb.clone());
        }

        if let Some(index) = db.olson_map.get("EUROPE/BERLIN") {
            // https://github.com/ropg/ezTime/blob/7b3c8aa020be818ac149e0762543ac5e81ccfabe/server/server#L113
            debug!("Overriding 'DE' to 'Europe/Berlin'");
            db.country_map.insert("DE".into(), vec![*index]);
        }

        if let Some(dublin) = db.lookup_olson_mut("EUROPE/DUBLIN") {
            // https://github.com/ropg/ezTime/blob/7b3c8aa020be818ac149e0762543ac5e81ccfabe/server/server#L152
            // https://github.com/ropg/ezTime/issues/65
            // https://github.com/ropg/ezTime/issues/159
            debug!("Rewriting timezone 'Europe/Dublin'");
            dublin.posix = "GMT0IST,M3.5.0/1,M10.5.0".into();
        }

        Ok(db)
    }

    fn refreshed_at(config: &Config) -> Option<SystemTime> {
        file_last_modified(config.data_path(POSIXINFO_FILE)).ok()
    }

    fn add_timezone(&mut self, olson: &str, posix: &str) -> Result<(), String> {
        let entry = Timezone {
            olson: olson.to_owned(),
            posix: posix.to_owned(),
        };
        let key = normalize_string(olson);
        if self.olson_map.contains_key(&key) {
            return Err(format!("Timezone '{}' already added to database", olson));
        }

        debug!("Adding timezone {} {}", olson, posix);
        self.timezones.push(entry);
        self.olson_map.insert(key, self.timezones.len() - 1);
        Ok(())
    }

    fn add_country_timezone(&mut self, country: &str, olson: &str) -> Result<(), String> {
        let index = self.olson_map.get(&normalize_string(olson)).ok_or(format!(
            "Attempted to add country '{}' to nonexistent timezone '{}'",
            country, olson
        ))?;

        let key = normalize_string(country);
        let vec = self.country_map.entry(key).or_insert(Vec::new());
        if vec.contains(index) {
            return Err(format!(
                "Country '{}' already contains timezone '{}'",
                country, olson
            ));
        }

        debug!("Adding country {} to {}", country, olson);
        vec.push(*index);
        Ok(())
    }

    fn lookup_olson(&self, normalized_olson: &str) -> Option<&Timezone> {
        self.olson_map
            .get(normalized_olson)
            .map(|index| self.timezones.get(*index))
            .flatten()
    }

    fn lookup_olson_mut(&mut self, normalized_olson: &str) -> Option<&mut Timezone> {
        self.olson_map
            .get(normalized_olson)
            .map(|index| self.timezones.get_mut(*index))
            .flatten()
    }

    fn lookup_country(&self, normalized_country: &str) -> Option<Vec<&Timezone>> {
        self.country_map.get(normalized_country).map(|indicies| {
            indicies
                .into_iter()
                .filter_map(|index| self.timezones.get(*index))
                .collect::<Vec<_>>()
        })
    }
}

struct GeoIpDb {
    reader: maxminddb::Reader<memmap::Mmap>,
}

impl GeoIpDb {
    async fn update(config: &Config) -> Result<(), Box<dyn Error>> {
        info!("Updating GeoIP database...");
        sh!(UPDATE_MMDB_SH_PATH, &config.data_dir, &config.mmdb_url).await
    }

    fn load(config: &Config) -> Result<Self, Box<dyn Error>> {
        let path = config.data_path(MMDB_CITY_FILE);
        let new_path = config.data_path(format!("{}.new", MMDB_CITY_FILE));
        info!("Loading GeoIP database from {}", path.display());
        if new_path.exists() {
            info!("Replacing database with {}", new_path.display());
            if let Err(err) = fs::rename(&new_path, &path) {
                error!("Failed to replace {}: {}", path.display(), err);
                error!("The existing database will be used instead");
            }
        }
        Ok(GeoIpDb {
            reader: maxminddb::Reader::open_mmap(path)?,
        })
    }

    fn refreshed_at(config: &Config) -> Option<SystemTime> {
        file_last_modified(config.data_path(format!("{}.new", MMDB_CITY_FILE)))
            .or_else(|_| file_last_modified(config.data_path(MMDB_CITY_FILE)))
            .ok()
    }

    fn lookup_timezone(&self, addr: IpAddr) -> Option<&str> {
        self.reader
            .lookup::<geoip2::City>(addr)
            .ok()
            .and_then(|city| city.location.and_then(|location| location.time_zone))
    }
}

fn normalize_string(request: &str) -> String {
    request.trim().to_uppercase().replace(' ', "_")
}

fn read_file_lines<P: AsRef<Path>>(filename: P) -> io::Result<impl Iterator<Item = String>> {
    let file = fs::File::open(filename.as_ref())?;
    Ok(io::BufReader::new(file)
        .lines()
        .filter_map(|line| line.ok()))
}

fn file_last_modified<P: AsRef<Path>>(filename: P) -> io::Result<SystemTime> {
    fs::metadata(filename.as_ref()).and_then(|metadata| metadata.modified())
}

#[derive(Debug)]
struct Config {
    rate_limit: Duration,
    client_prune_period: Duration,
    tz_refresh_period: Duration,
    geoip_refresh_period: Duration,
    data_dir: PathBuf,
    host: String,
    port: u16,
    #[cfg(feature = "metrics")]
    metrics_host: String,
    #[cfg(feature = "metrics")]
    metrics_port: u16,
    mmdb_url: String,
}

impl Config {
    fn load() -> Result<Self, String> {
        Ok(Config {
            rate_limit: Duration::from_millis(Self::getenv("TZD_RATELIMIT_MS", Some(3000))?),
            client_prune_period: Duration::from_secs(Self::getenv(
                "TZD_CLIENT_PRUNE_SECONDS",
                Some(10),
            )?),
            tz_refresh_period: Duration::from_secs(
                Self::getenv("TZD_TZ_REFRESH_DAYS", Some(7))? * SECONDS_PER_DAY,
            ),
            geoip_refresh_period: Duration::from_secs(
                Self::getenv("TZD_GEOIP_REFRESH_DAYS", Some(7))? * SECONDS_PER_DAY,
            ),
            data_dir: Self::getenv::<PathBuf>("TZD_DATA_DIR", Some("/home/timezoned".into()))?,
            host: Self::getenv::<String>("TZD_HOST", Some("0.0.0.0".into()))?,
            port: Self::getenv::<u16>("TZD_PORT", Some(2342))?,
            #[cfg(feature = "metrics")]
            metrics_host: Self::getenv::<String>("TZD_METRICS_HOST", Some("0.0.0.0".into()))?,
            #[cfg(feature = "metrics")]
            metrics_port: Self::getenv::<u16>("TZD_METRICS_PORT", Some(0))?,
            mmdb_url: Self::getenv::<String>("TZD_MMDB_URL", Some("".into()))?,
        })
    }

    fn data_path<P: AsRef<Path>>(&self, p: P) -> PathBuf {
        self.data_dir.join(p)
    }

    fn getenv<T: FromStr>(key: &str, default: Option<T>) -> Result<T, String> {
        match std::env::var(key) {
            Ok(value) => value.parse::<T>().map_err(|_| {
                format!(
                    "{} is configured with invalid value '{}', expected {}",
                    key,
                    value,
                    std::any::type_name::<T>()
                )
            }),
            Err(_) => {
                if let Some(default) = default {
                    Ok(default)
                } else {
                    Err(format!("{} was not specified", key))
                }
            }
        }
    }
}

fn interval(last_ran_at: Option<SystemTime>, period: Duration) -> Interval {
    let time_since_run = match last_ran_at {
        Some(time) => SystemTime::now().duration_since(time).unwrap_or(period),
        None => period,
    };

    let mut interval = interval_at(
        if time_since_run < period {
            Instant::now() + period - time_since_run
        } else {
            Instant::now()
        },
        period,
    );
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    interval
}

fn ok(tz: &Timezone) -> String {
    format!("OK {} {}", tz.olson, tz.posix)
}

macro_rules! log_request {
    ($type:expr$(, $label:expr => $value:expr)*) => {
        #[cfg(feature = "metrics")]
        metrics::increment_counter!("requests", "type" => $type$(, $label => $value)*);
    };
}

#[allow(unused_must_use)]
async fn run() -> Result<(), Box<dyn Error>> {
    info!("Initializing");

    let config = Config::load()?;
    debug!("{:#?}", config);
    if config.rate_limit.is_zero() {
        warn!("Rate-limiting is disabled");
    }

    let mut timezones = match TimezoneDb::load(&config) {
        Ok(timezones) => timezones,
        Err(err) => {
            warn!("Could not load timezone database: {}", err);
            warn!("Timezone database must first be loaded before the server can accept requests");
            TimezoneDb::update(&config)
                .await
                .map_err(|err| format!("Timezone database refresh failed: {}", err))?;
            TimezoneDb::load(&config)
                .map_err(|err| format!("Could not initialize timezone database: {}", err))?
        }
    };

    let timezone_refresh_task = unfold(
        interval(TimezoneDb::refreshed_at(&config), config.tz_refresh_period),
        |mut interval| async {
            interval.tick().await;
            Some((TimezoneDb::update(&config).await, interval))
        },
    );
    pin!(timezone_refresh_task);

    let mut geoip = match GeoIpDb::load(&config) {
        Ok(geoip) => Some(geoip),
        Err(err) => {
            warn!("Could not load GeoIP database: {}", err);
            if config.mmdb_url.len() > 0 {
                warn!(
                    "Until the GeoIP database is loaded, every GeoIP request will return '{}'",
                    String::from_utf8_lossy(ERR_TIMEZONE_NOT_FOUND)
                );
            } else {
                warn!(
                    "GeoIP database refresh is disabled. Every GeoIP request will return '{}'",
                    String::from_utf8_lossy(ERR_TIMEZONE_NOT_FOUND)
                );
            }
            None
        }
    };

    let geoip_refresh_task = unfold(
        interval(GeoIpDb::refreshed_at(&config), config.geoip_refresh_period),
        |mut interval| async {
            interval.tick().await;
            Some((GeoIpDb::update(&config).await, interval))
        },
    );
    pin!(geoip_refresh_task);

    let mut client_prune_interval = interval(Some(SystemTime::now()), config.client_prune_period);

    info!("Binding UDP socket {}:{}", config.host, config.port);
    let socket = UdpSocket::bind(format!("{}:{}", config.host, config.port)).await?;
    let mut clients = HashMap::<IpAddr, Instant>::new();
    let mut buf = [0u8; MAX_REQUEST_SIZE];

    #[cfg(feature = "metrics")]
    if config.metrics_port > 0 {
        info!(
            "Initializing prometheus exporter on {}:{}/metrics",
            config.metrics_host, config.metrics_port
        );
        metrics_exporter_prometheus::PrometheusBuilder::new()
            .with_http_listener(std::net::SocketAddr::new(
                IpAddr::from_str(&config.metrics_host)?,
                config.metrics_port,
            ))
            .install()?;

        metrics::describe_counter!("requests", "Total requests received by the server");
    }

    info!("Server is ready");

    loop {
        select! {
            biased;
            // Reload timezone data
            Some(result) = timezone_refresh_task.next() => match result {
                Ok(()) => match TimezoneDb::load(&config) {
                    Ok(new_timezones) => {
                        info!("Timezone database refresh complete");
                        timezones = new_timezones;
                    },
                    Err(err) => {
                        error!("Timezone database refresh completed successfully, but the new data could not be loaded");
                        error!("Cause: {}", err);
                    },
                },
                Err(err) => error!("Timezone database refresh failed: {}", err),
            },
            // Reload GeoIP data
            Some(result) = geoip_refresh_task.next(), if config.mmdb_url.len() > 0 => match result {
                Ok(()) => match GeoIpDb::load(&config) {
                    Ok(new_geoip) => {
                        info!("GeoIP database refresh complete");
                        geoip.replace(new_geoip);
                    },
                    Err(err) => {
                        error!("GeoIP database refresh completed successfully, but the new data could not be loaded");
                        error!("Cause: {}", err);
                    },
                },
                Err(err) => error!("GeoIP database refresh failed: {}", err),
            },
            // Prune clients that haven't sent requests within the rate limit window every client_prune_interval
            now = client_prune_interval.tick() => {
                clients.retain(|_, last_activity| {
                    now - *last_activity < config.rate_limit
                });
            },
            // UDP request handler
            Ok((len, addr)) = socket.recv_from(&mut buf) => {
                let now = Instant::now();

                // Don't respond to clients sending requests over MAX_REQUEST_SIZE
                if len == MAX_REQUEST_SIZE {
                    log_request!("too_large");
                    continue;
                }

                // Don't respond to rate limited clients
                if let Some(last_client_message) = clients.get(&addr.ip()) {
                    if now - *last_client_message < config.rate_limit {
                        log_request!("rate_limited");
                        continue;
                    }
                }
                clients.insert(addr.ip(), now);

                // Process request
                let request = normalize_string(&String::from_utf8_lossy(&buf[..len]));

                if request.len() == 2 {
                    // 2-letter country code lookup
                    match timezones.lookup_country(&request) {
                        Some(tzs) => if tzs.len() == 1 {
                            log_request!("country", "country" => request, "timezone" => tzs[0].olson.to_owned());
                            socket.send_to(ok(tzs[0]).as_bytes(), addr).await
                        } else {
                            log_request!("country", "country" => request, "timezone" => "not_found");
                            socket.send_to(ERR_COUNTRY_SPANS_MULTIPLE_TIMEZONES, addr).await
                        },
                        None => {
                            log_request!("country", "country" => "not_found");
                            socket.send_to(ERR_COUNTRY_NOT_FOUND, addr).await
                        },
                    };
                } else if request == "GEOIP" {
                    let Some(geoip) = &geoip else {
                        // GeoIP database is not available
                        log_request!("geoip", "timezone" => "not_found");
                        socket.send_to(ERR_GEOIP_LOOKUP_FAILED, addr).await;
                        continue;
                    };

                    // GeoIP lookup
                    match geoip.lookup_timezone(addr.ip()).and_then(
                        |olson| timezones.lookup_olson(&normalize_string(olson))
                    ) {
                        Some(tz) => {
                            log_request!("geoip", "timezone" => tz.olson.to_owned());
                            socket.send_to(ok(tz).as_bytes(), addr).await
                        },
                        None => {
                            log_request!("geoip", "timezone" => "not_found");
                            socket.send_to(ERR_GEOIP_LOOKUP_FAILED, addr).await
                        },
                    };
                } else {
                    // Olson name lookup
                    match timezones.lookup_olson(&request) {
                        Some(tz) => {
                            log_request!("timezone", "timezone" => tz.olson.to_owned());
                            socket.send_to(ok(tz).as_bytes(), addr).await
                        },
                        None => {
                            log_request!("timezone", "timezone" => "not_found");
                            socket.send_to(ERR_TIMEZONE_NOT_FOUND, addr).await
                        },
                    };
                }
            }
        };
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    pretty_env_logger::init_custom_env("TZD_LOG");

    match run().await {
        Ok(_) => info!("Server has shut down"),
        Err(err) => error!("{}", err),
    };
}
