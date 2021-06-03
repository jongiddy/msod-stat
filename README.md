# msod-stat
Display useful information about OneDrive, including total disk usage and duplicate files.

This has only been tested on OneDrive Personal, but should work for OneDrive Business and Document Libraries.

Example run:

```
$ cargo run --release

Drive 33939c2f5f6e
total:        1029.500 GiB
free:          778.295 GiB
used:          251.205 GiB = 24.40% (including 0.491 MiB pending deletion)
folders:     10101
files:      107700
duplicates:
1.198 GiB
	Pictures/Family Photos/2015/110_FUJI/DSCF0104.MOV
	Pictures/2015/110_FUJI/DSCF0104.MOV
667.240 MiB
	Pictures/Camera Roll/VID_20170523_200350.mp4
	Pictures/Wileyfox/VID_20170523_200350.mp4
482.585 MiB
	Pictures/Family Photos/2008/P1010769.MOV
	Pictures/Photos/2008/P1010769.MOV
```
