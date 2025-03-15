from pyinfra.operations import apt, pip, server
from pyinfra import host
from pyinfra.facts.server import Which
from pyinfra.api import FactBase

# Fact checkers
class k0sStarted(FactBase):
    '''
    Returns a boolean indicating whether k0s is started.
    '''

    command = 'k0s status'

    def process(self, output):
        return output

# Main logic
is_k0s_installed = host.get_fact(Which, command='k0s')

if not is_k0s_installed:
    server.shell(
        'curl -sSf https://get.k0s.sh | sh',
        _sudo=True
    )

    server.shell(
        'k0s install controller',
        _sudo=True
    )

if not host.get_fact(k0sStarted):
    server.shell(
        'k0s start',
        _sudo=True
    )
