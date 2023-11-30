# clrigctl

This is a small application which provides CAT support for [Cloudlog](https://github.com/magicbug/Cloudlog). It reads data from [Flrig](http://www.w1hkj.com/) and sends it to your Cloudlog instance.

## Supported Platforms
clrigctl is developed on **Linux** and has not yet been adapted for Windows or other operating systems. Please let me know if you are interested and I could have a look …

## Installation
Simply clone the git repository, compile it and copy the executable to where you want it to be, for example:
```
$ git clone https://git.rustysoft.de/martin/clrigctl.git
$ cd clrigctl
$ cargo build --release
$ sudo cp target/release/clrigctl ~/.local/bin/
```

Copy the example config file `clrigctl.toml` to `$HOME/.config/` and adapt it to your needs.
```
# This is an example config file. Please edit it to your needs
# and place it, for example, in your `$HOME/.config/`

[cloudlog]
# Note: URL should end with "/index.php/api/radio".
url = "https://cloudlog.example.com/index.php/api/radio"
key = "clxxxxxxxxxxxxx"
identifier = "clrigctl"

[flrig]
# Note: Do not forget the "http://".
host = "http://127.0.0.1"
port = "12345"
```

If you want to run clrigctl always in the background, you can copy the example systemd service file `clrigctl.service` to `$HOME/.config/systemd/user/` and adapt it (at least use the correct path to the binary!).
```
[Unit]
Description=Cloudlog CAT Control

[Service]
RestartSec=2s
Type=simple
ExecStart=/home/MYUSER/.local/bin/clrigctl
Restart=always
#Environment=RUST_LOG=Debug

[Install]
WantedBy=default.target
```

After a `systemctl --user daemon-reload` you can enable (and start) the service with `systemctl --user enable --now clrigctl.service`. 

clrigctl is running is then running in the background. It will just do nothing if there is no Flrig instance running. 

## Feedback welcome
Any feedback is very welcome. Please let me know whether the program was useful for you or if there are perhaps any suggestions or bugs. You can reach me on Matrix Chat (**@nnmcm:darc.de**), via the Fediverse (**@DG2SMB@social.darc.de**) or just plain old E-Mail [dg2smb@darc.de]("mailto:dg2smb@darc.de").

73  
Martin, DG2SMB