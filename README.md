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
* Run `bash bootstrap.sh`.
* Follow the prompts.

That's it. This script will make sure Python and other dependencies are installed, then setup Sparx.

## what does it do?
* Installs k0s, either locally or on a remote machine or set of machines.
* Configures the k0s cluster with Antrea CNI.
* Installs Tinkerbell (https://tinkerbell.org/)
* Configures Tinkerbell to deploy to any node that PXE boots from it.
