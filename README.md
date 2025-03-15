# sparx
systems management software

## goals
- bootstrap from bare metal
- self hosted
- simple to deploy
- easy to maintain
- easy to extend and hack on
- customisable

## getting started
- Install Ubuntu 22.04 LTS on amd64 hardware

- Install pyinfra on your control machine
```bash
sudo apt update
sudo apt install python3 python3-pip
sudo pip3 install --upgrade pip
sudo pip3 install pyinfra
```

- Clone the sparx repository
```bash
git clone https://github.com/sparx-systems/sparx.git
cd sparx
```

- Install k0s on the first node
```bash
curl -sSf https://get.k0s.sh | sudo sh
sudo k0s install controller
sudo k0s start # wait a minute
```


