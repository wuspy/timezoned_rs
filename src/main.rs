use futures::future::OptionFuture;
use log::{debug, error, info, warn};
use maxminddb::geoip2;
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io::{self, BufRead};
use std::net::IpAddr;
use std::str::FromStr;
use std::time::SystemTime;
use tokio::net::UdpSocket;
use tokio::select;
use tokio::task::JoinHandle;
use tokio::time::{interval_at, Duration, Instant, Interval, MissedTickBehavior};

const ERR_TIMEZONE_NOT_FOUND: &[u8] = "ERROR Timezone Not Found".as_bytes();
const ERR_GEOIP_LOOKUP_FAILED: &[u8] = "ERROR GeoIP Lookup Failed".as_bytes();
const ERR_COUNTRY_NOT_FOUND: &[u8] = "ERROR Country Not Found".as_bytes();
const ERR_COUNTRY_SPANS_MULTIPLE_TIMEZONES: &[u8] =
    "ERROR Country Spans Multiple Timezones".as_bytes();

const MAX_REQUEST_SIZE: usize = 512;
const SECONDS_PER_DAY: u64 = 86400;

const UPDATE_TZDATA_SH_PATH: &str = "./update_tzdata.sh";
const UPDATE_MMDB_SH_PATH: &str = "./update_mmdb.sh";
const POSIXINFO_PATH: &str = "/home/timezoned/posixinfo";
const ZONETAB_PATH: &str = "/home/timezoned/zone1970.tab";
const MMDB_CITY_PATH: &str = "/home/timezoned/GeoLite2-City.mmdb";

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
    async fn update() -> Result<(), String> {
        info!("Updating timezone database...");
        run_script(UPDATE_TZDATA_SH_PATH, []).await
    }

    fn load() -> Result<Self, String> {
        let mut db = TimezoneDb {
            timezones: Vec::new(),
            olson_map: HashMap::new(),
            country_map: HashMap::new(),
        };

        info!("Loading timezones from {}", POSIXINFO_PATH);
        for line in read_file_lines(POSIXINFO_PATH)? {
            let [olson, posix] = line.split_whitespace().collect::<Vec<_>>()[..] else {
                warn!("posixinfo entry is improperly formatted, skipping: {}", line);
                continue;
            };
            db.add_timezone(olson, posix)?;
        }
        info!("{} timezones loaded", db.timezones.len());

        // Read countries
        info!("Loading countries from {}", ZONETAB_PATH);
        for line in read_file_lines(ZONETAB_PATH)? {
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

    fn refreshed_at() -> Option<SystemTime> {
        file_last_modified(POSIXINFO_PATH).ok()
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
    reader: maxminddb::Reader<Vec<u8>>,
}

impl GeoIpDb {
    async fn update(mmdb_url: &str) -> Result<(), String> {
        info!("Updating GeoIP database...");
        run_script(UPDATE_MMDB_SH_PATH, [mmdb_url]).await
    }

    fn load() -> Result<Self, Box<dyn Error>> {
        info!("Loading GeoIP database from {}", MMDB_CITY_PATH);
        Ok(GeoIpDb {
            // TODO maybe use mmap?
            reader: maxminddb::Reader::open_readfile(MMDB_CITY_PATH)?,
        })
    }

    fn refreshed_at() -> Option<SystemTime> {
        file_last_modified(MMDB_CITY_PATH).ok()
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

fn read_file_lines(filename: &str) -> Result<impl Iterator<Item = String>, String> {
    let file = fs::File::open(filename)
        .map_err(|err| format!("Failed to open from '{}': {}", filename, err))?;
    Ok(io::BufReader::new(file)
        .lines()
        .filter_map(|line| line.ok()))
}

fn file_last_modified(filename: &str) -> Result<SystemTime, String> {
    fs::metadata(filename)
        .and_then(|metadata| metadata.modified())
        .map_err(|err| format!("Failed to read time modified for '{}': {}", filename, err))
}

async fn run_script<'a, I>(filename: &'a str, args: I) -> Result<(), String>
where
    I: IntoIterator<Item = &'a str>,
{
    use async_process::Command;

    info!("sh {}", filename);
    let status = Command::new("sh")
        .arg(filename)
        .args(args)
        .status()
        .await
        .map_err(|err| format!("{}", err))?;

    if !status.success() {
        return Err(format!("{}", status));
    }

    Ok(())
}

#[derive(Debug)]
struct Config {
    rate_limit: Duration,
    client_prune_period: Duration,
    tz_refresh_period: Duration,
    geoip_refresh_period: Duration,
    host: String,
    port: u16,
    mmdb_url: String,
}

impl Config {
    fn load() -> Result<Self, String> {
        Ok(Config {
            rate_limit: Duration::from_secs(Self::getenv("TZD_RATELIMIT_SECONDS", Some(3))?),
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
            host: Self::getenv::<String>("TZD_HOST", Some("127.0.0.1".into()))?,
            port: Self::getenv::<u16>("TZD_PORT", Some(2342))?,
            mmdb_url: Self::getenv::<String>("TZD_GEOIP_URL", Some("".into()))?,
        })
    }

    fn getenv<T>(key: &str, default: Option<T>) -> Result<T, String>
    where
        T: FromStr,
    {
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

fn create_refresh_interval(last_refreshed: Option<SystemTime>, period: Duration) -> Interval {
    let time_since_refresh = match last_refreshed {
        Some(time) => SystemTime::now().duration_since(time).unwrap_or(period),
        None => period,
    };

    let mut interval = interval_at(
        if time_since_refresh < period {
            Instant::now() + period - time_since_refresh
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

#[allow(unused_must_use)]
async fn run() -> Result<(), Box<dyn Error>> {
    info!("Initializing");

    let config = Config::load()?;
    debug!("{:#?}", config);
    if config.rate_limit.is_zero() {
        warn!("Rate-limiting is disabled");
    }

    let mut timezones = match TimezoneDb::load() {
        Ok(timezones) => timezones,
        Err(err) => {
            warn!("Could not load timezone database: {}", err);
            warn!("Timezone database must first be loaded before the server can accept requests");
            TimezoneDb::update()
                .await
                .map_err(|err| format!("Timezone database refresh failed: {}", err))?;
            TimezoneDb::load()
                .map_err(|err| format!("Could not initialize timezone database: {}", err))?
        }
    };

    let mut geoip = match GeoIpDb::load() {
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

    let mut tz_refresh_interval =
        create_refresh_interval(TimezoneDb::refreshed_at(), config.tz_refresh_period);

    let mut geoip_refresh_interval =
        create_refresh_interval(GeoIpDb::refreshed_at(), config.geoip_refresh_period);

    let mut client_prune_interval =
        create_refresh_interval(Some(SystemTime::now()), config.client_prune_period);

    let mut tz_refresh_task: OptionFuture<JoinHandle<_>> = None.into();
    let mut geoip_refresh_task: OptionFuture<JoinHandle<_>> = None.into();

    let socket = UdpSocket::bind(format!("{}:{}", config.host, config.port)).await?;
    let mut clients: HashMap<IpAddr, Instant> = HashMap::new();
    let mut buf = [0u8; MAX_REQUEST_SIZE];

    info!("Server is listening on {}", socket.local_addr().unwrap());

    loop {
        select! {
            biased;
            // Refresh timezone data asynchronously every tz_refresh_interval
            _ = tz_refresh_interval.tick() => {
                tz_refresh_task = Some(tokio::spawn(TimezoneDb::update())).into();
            },
            Some(result) = &mut tz_refresh_task => {
                tz_refresh_task = None.into(); // Clear completed task
                match result {
                    Ok(Ok(_)) => match TimezoneDb::load() {
                        Ok(new_timezones) => {
                            info!("Timezone database refresh complete");
                            timezones = new_timezones;
                        },
                        Err(err) => {
                            error!("Timezone database refresh completed successfully, but the new data could not be loaded");
                            error!("Cause: {}", err);
                        },
                    },
                    Ok(Err(err)) => error!("Timezone database refresh failed: {}", err),
                    Err(err) => std::panic::resume_unwind(err.into_panic()),
                }
            },
            // Refresh GeoIP data asynchronously every geoip_refresh_interval
            _ = geoip_refresh_interval.tick() => {
                let mmdb_url = config.mmdb_url.to_owned();
                // Don't start job if mmdb_url isn't configured
                if mmdb_url.len() > 0 {
                    geoip_refresh_task = Some(tokio::spawn(async move {
                        GeoIpDb::update(mmdb_url.as_str()).await
                    })).into();
                }
            },
            Some(result) = &mut geoip_refresh_task => {
                geoip_refresh_task = None.into(); // Clear completed task
                match result {
                    Ok(Ok(_)) => match GeoIpDb::load() {
                        Ok(new_geoip) => {
                            info!("GeoIP database refresh complete");
                            geoip.replace(new_geoip);
                        },
                        Err(err) => {
                            error!("GeoIP database refresh completed successfully, but the new data could not be loaded");
                            error!("Cause: {}", err);
                        },
                    },
                    Ok(Err(err)) => error!("GeoIP database refresh failed: {}", err),
                    Err(err) => std::panic::resume_unwind(err.into_panic()),
                }
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
                    continue;
                }

                // Don't respond to rate limited clients
                if let Some(last_client_message) = clients.get(&addr.ip()) {
                    if now - *last_client_message < config.rate_limit {
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
                            socket.send_to(ok(tzs[0]).as_bytes(), addr).await
                        } else {
                            socket.send_to(ERR_COUNTRY_SPANS_MULTIPLE_TIMEZONES, addr).await
                        },
                        None => socket.send_to(ERR_COUNTRY_NOT_FOUND, addr).await,
                    };
                } else if request == "GEOIP" {
                    let Some(geoip) = &geoip else {
                        // GeoIP database is not available
                        socket.send_to(ERR_GEOIP_LOOKUP_FAILED, addr).await;
                        continue;
                    };

                    // GeoIP lookup
                    match geoip.lookup_timezone(addr.ip()).and_then(
                        |olson| timezones.lookup_olson(&normalize_string(olson))
                    ) {
                        Some(tz) => socket.send_to(ok(tz).as_bytes(), addr).await,
                        None => socket.send_to(ERR_GEOIP_LOOKUP_FAILED, addr).await,
                    };
                } else {
                    // Olson name lookup
                    match timezones.lookup_olson(&request) {
                        Some(tz) => socket.send_to(ok(tz).as_bytes(), addr).await,
                        None => socket.send_to(ERR_TIMEZONE_NOT_FOUND, addr).await,
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
