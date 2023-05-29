#!/bin/sh

DIR=$1

echo update_tzdata: Using data directory $DIR

cd $DIR || exit 1

rm -rf zoneinfo
mkdir zoneinfo || exit 1

rm -rf tzdata
mkdir tzdata && cd tzdata || exit 1

wget -nv ftp://ftp.iana.org/tz/tzdata-latest.tar.gz || exit 1
tar zxf tzdata-latest.tar.gz || exit 1
rm tzdata-latest.tar.gz
mv zone1970.tab $DIR
for i in africa antarctica asia australasia etcetera europe northamerica southamerica; do
	zic -d $DIR/zoneinfo $i;
done

cd $DIR
rm posixinfo
cd zoneinfo
for i in `find *|grep /`
do
	if [ -f $i ]; then
		echo -n $i  >> $DIR/posixinfo
		echo -n " " >> $DIR/posixinfo
		tail -1 $i  >> $DIR/posixinfo
	fi
done

cd $DIR
rm -rf zoneinfo
rm -rf tzdata
