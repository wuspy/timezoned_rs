#!/bin/sh

rm -rf ~timezoned/mmdb
mkdir ~timezoned/mmdb

cd ~timezoned/mmdb || exit 1

wget $1 || exit 1

if [ -f *.tar.gz ]; then
	tar zxf *.tar.gz
	rm *.tar.gz
fi

if [ ! -f GeoLite2-City.mmdb ]; then
	echo "GeoLite2-City.mmdb (or a .tar.gz archive of it) could not be found in the data downloaded"
	exit 1
fi

mv GeoLite2-City.mmdb ~timezoned
cd ~timezoned
rm -rf ~timezoned/mmdb

