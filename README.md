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
Pick a node to be your bootstrap machine.

* Run `bash bootstrap.sh`.
* Follow the prompts.

That's it. This script will make sure Python and other dependencies are installed, then setup Sparx.

## what does it do?
* Installs k3s on a bootstrap machine
* Sets up Tinkerbell to adopt and manage additional machines automatically
* Configures Tinkerbell to deploy to any node that PXE boots from it.
