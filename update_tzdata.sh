#!/bin/sh

rm -rf ~timezoned/tzdata
mkdir ~timezoned/tzdata

rm -rf ~timezoned/zoneinfo
mkdir ~timezoned/zoneinfo

cd ~timezoned/tzdata || exit 1

wget ftp://ftp.iana.org/tz/tzdata-latest.tar.gz || exit 1
tar zxf tzdata-latest.tar.gz || exit 1
rm tzdata-latest.tar.gz
mv zone1970.tab ~timezoned
for i in africa antarctica asia australasia etcetera europe northamerica southamerica; do
	zic -d ~timezoned/zoneinfo $i;
done

rm ~timezoned/posixinfo

cd ~timezoned/zoneinfo
for i in `find *|grep /`
do
	if [ -f $i ]; then
		echo -n $i  >> ~timezoned/posixinfo
		echo -n " " >> ~timezoned/posixinfo
		tail -1 $i  >> ~timezoned/posixinfo
	fi
done

cd ~timezoned
rm -rf ~timezoned/zoneinfo
rm -rf ~timezoned/tzdata

