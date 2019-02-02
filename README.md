# msod-stat
Display useful information about OneDrive, including total disk usage and duplicate files.

Example run:

```
$ cargo run < creds
..................................................................................................................
..................................................................................................................
..................................................................................................................
..................................................................................................................
..................................................................................................................
....................
Drive 33939c2f5f6e
folders:     10101
files:      107696 (251.195 GiB)
total:        1029.500 GiB
free:          778.305 GiB
used:          251.195 GiB = 24.40% (including 0.491 MiB pending deletion)
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

## Creating a `creds` file

See https://docs.microsoft.com/en-us/onedrive/developer/rest-api/getting-started/graph-oauth

To get a username/password for an app:
1. Go to https://apps.dev.microsoft.com/
2. Click Add an App.
3. Skip the guided setup.
4. Set Web Redirect URL to http://localhost:3003/redirect
5. Add Delegated Permissions of Files.Read.All
6. Copy the Application Id as the username.
7. Click Generate New Password.
8. Copy the password.
9. Create a credentials file containing the username and password on separate lines.
10. Pipe the credentials file into this command.
    
