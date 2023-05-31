# timezoned_rs

This is a Rust implementation of a timezoned server from the [ezTime](https://github.com/ropg/ezTime) project, designed primarily to be lower maintenance and easier to deploy than the original PHP version.

## Cool, but what is this?

[Timezoned](https://github.com/ropg/ezTime/tree/master/server) is a simple UDP service that accepts requests for timezone names (`America/Chicago`), country codes (`NL`), or geoip requests (`GeoIP`) and returns the POSIX information for that timezone/country/geolocation, like the GMT offset, the rules for when DST starts and ends, or if it has DST at all.

Storing this information for *every timezone* on a embedded/IoT device can take up a lot of resources, nevermind the fact that these rules are convoluted and constantly changing making it impractical to store them on a device which may never recieve future updates.

There are other attempts to solve this problem, like [Adafruit's server](https://learn.adafruit.com/adafruit-magtag/getting-the-date-time) and [WorldTimeApi](http://worldtimeapi.org/), but neither will give you the actual POSIX rules for a timezone, and Adafruit's even requires an API key. In my opinion, timezoned is a better solution to this problem because it provides you with all the information you need to keep track of the time *and date* on your own, and it can get away with not needing an API key by being lightweight (UDP instead of TCP) and having aggressive rate limiting.

## Features

- Low resource usage
    - ~500KB RAM
    - ~100MB docker image
    - ~80MB persistent volume with geoip, or <1MB persistent volume without geoip
    - single-threaded
- Auto-updating timezone and geoip databases with zero downtime
- ~5x the throughput of Rop's PHP implementation
- Optional prometheus metrics, if you're into that kind of thing

# Installation

**TODO**

ghcr container builds and docker compose examples will be added after a stable release

In the meantime, development builds are hosted at `timezoned.jacobjordan.tech:2342` if you want to test it out without building your own

# Configuration options

Configuration is done through environment variables.

| Variable | Default | Description |
| -------- | ------- | ----------- |
| `TZD_RATELIMIT_MS` | `3000` | Client rate limiting. A value of `3000` means an IP address will only be reponded to once every 3 seconds. This is the same value used by upstream timezoned and is recommended. A value of `0` will disable rate limiting, and can be used if timezoned is behind a reverse proxy and you insist on using its rate limiting instead.  |
| `TZD_CLIENT_PRUNE_SECONDS` | `10` | How often the list of client IPs is pruned to remove clients that haven't sent requests within the rate limiting window. |
| `TZD_TZ_REFRESH_DAYS` | `7` | How often the timezone database should be refreshed from [iana.org](iana.org). |
| `TZD_GEOIP_REFRESH_DAYS` | `7` | How often the MaxMind GeoLite2 database should be refreshed from the source configured in `TZD_MMDB_URL`. |
| `TZD_MMDB_URL` | (none) | A URL that provides a MaxMind GeoLite2 City database, either uncompressed or in .tar.gz format. If left unset, then GeoIP lookups will be disabled and every GeoIP request will return `ERROR GeoIP Lookup Failed`. If you have a MaxMind account and API key, this URL would be `https://download.maxmind.com/app/geoip_download?edition_id=GeoLite2-City&license_key=(your_api_key)&suffix=tar.gz` |
| `TZD_DATA_DIR` | `/home/timezoned` | Persistent data directory. |
| `TZD_HOST` | `0.0.0.0` | Host address to bind to. |
| `TZD_PORT` | `2342` | Host port to bind to. |
| `TZD_METRICS_HOST` | `0.0.0.0` | Host address to bind to for the prometheus metrics service. |
| `TZD_METRICS_PORT` | (none) | Host port to bind to for the prometheus metrics service. If left unset, then metrics will be disabled. | 
| `TZD_LOG` | `info` | Log verbosity. Supported values are `error`, `warn`, `info`, `debug`, and `trace`. A value of `info` is recommended for most deployments.

