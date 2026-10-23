# Air traffic

Rerun's `air_traffic_data` example on its 10-minute dataset: ADS-B aircraft positions over Germany, Switzerland and the surrounding region, above the region outlines.

## Start and stop

```sh
uv run demos/air_traffic/run.py
```

On the QAROS PC, add the viewer's QAROS options after `--`, for example `-- --sink qaros --pairing-code <PAIRING-CODE> --panels 0.0.0.0:9877`. Ctrl+C or closing the viewer window stops it.

The first start downloads the aircraft data (2.7 MB) and the region outlines (30 MB) and records them once, about 4 minutes on an Apple M5 Pro; later starts replay the cached 9 MB recording in seconds without network. Ten minutes of traffic play at 10x, so one loop is a minute. The map panel loads OpenStreetMap tiles and needs internet; the 3D view does not.

## What it shows

- Each dot in the 3D view is an aircraft with a trail of its last minute of flight. Its height is its altitude, exaggerated 10x by the example so climbs and descents are visible.
- The red lines are the NUTS region outlines.
- The map panel shows the same aircraft on OpenStreetMap at zoom 7.

## In the glasses

The volume lies flat on the table, with the map panel to the right of it, north up in both. Crouch to table level to see the traffic separate by altitude.

## If something looks wrong

- The map panel is grey: no internet. The 3D view is unaffected.
- Aircraft look cluttered or the frame rate is low: shorter trails help both. Trails are 60 s of data by default; set `AIR_TRAFFIC_TRAIL_SECONDS` (for example 30, or 120 for longer trails) in the environment.

## Data

ADS-B data that [INVOLI](https://www.involi.com) provided for Rerun's `air_traffic_data` example, hosted by Rerun ([involi_demo_set_1_10min.zip](https://storage.googleapis.com/rerun-example-datasets/involi/involi_demo_set_1_10min.zip)); the example does not state a licence for it. Region outlines from Eurostat GISCO, NUTS 2021 ([ref-nuts-2021-01m.json.zip](https://gisco-services.ec.europa.eu/distribution/v2/nuts/download/ref-nuts-2021-01m.json.zip)), under the copyright notice of [Eurostat GISCO](https://ec.europa.eu/eurostat/web/gisco/geodata/statistical-units/territorial-units-statistics). Map tiles © [OpenStreetMap contributors](https://www.openstreetmap.org/copyright).
