# sourced by /init: writes the raw user-data to $1
userdata_fetch() { wget -q -O "$1" http://169.254.169.254/hetzner/v1/userdata; }
