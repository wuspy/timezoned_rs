#!/bin/sh

DIR=$1
URL=$2

echo update_mmdb: Using data directory $DIR

cd $DIR || exit 1

rm -rf mmdb
mkdir mmdb && cd mmdb || exit 1

wget -nv $URL || exit 1

if [ -f *.tar.gz ]; then
	tar zxf *.tar.gz
	rm *.tar.gz
fi

if [ ! -f GeoLite2-City.mmdb ]; then
	echo "update_mmdb: GeoLite2-City.mmdb (or a .tar.gz archive of it) could not be found in the data downloaded"
	exit 1
fi

mv GeoLite2-City.mmdb $DIR/GeoLite2-City.mmdb.new || exit 1
cd $DIR
rm -rf mmdb
