# 3D map pictures

`run.py` renders five fixed views of the 3D map with the real app against a
simulated aircraft (`python/tools/scene.py`) and compares them with `ref/*.jpg`.
`make_city.py` seeds a sandbox with 515 buildings (the sandbox blocks Overpass),
the same cells the real `BuildingService` makes from an Overpass answer.

```bash
cargo build --release
python3 src/map3d/shots/run.py             # compare
python3 src/map3d/shots/run.py --update    # after a change you meant to make: look at them, then commit
```

The views: `flat_city` (relief only over a city), `alps_relief`, `far_haze`
(distant ground fading into the haze), `ridge_in_front` (a ridge hiding the
valley behind it, the depth test at work) and `buildings`. Differences of
more than 4 percent of the pixels fail; a failing view writes `<name>.new.png`
here to look at. Needs the network (imagery and elevation tiles) and Pillow.
