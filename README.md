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

**TODO**
