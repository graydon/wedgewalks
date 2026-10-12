#!/bin/sh
# Download pre-cut city extracts from BBBike into data/<city>.osm.pbf.
# Usage: scripts/fetch.sh Vancouver Toronto London Paris
# BBBike city names: https://download.bbbike.org/osm/bbbike/
# For cities BBBike lacks, take a Geofabrik region and cut it with
#   osmium extract -b <lon1,lat1,lon2,lat2> region.osm.pbf -o data/<city>.osm.pbf
set -eu
mkdir -p data
for c in "$@"; do
  out="data/$(echo "$c" | tr 'A-Z ' 'a-z_').osm.pbf"
  echo "$c -> $out"
  curl -fL --progress-bar -o "$out" "https://download.bbbike.org/osm/bbbike/$c/$c.osm.pbf"
done
