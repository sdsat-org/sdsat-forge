set minimum-version := '1.56.0'
set unstable
set lists
set default-script
set positional-arguments

[windows]
set script-interpreter := ['powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File']

forge := 'cargo run --locked --bin forge --'
[windows]
forward := '@args; if ($LASTEXITCODE) { exit $LASTEXITCODE }'

[unix]
forward := '"$@"'
export CARGO_BUILD_JOBS := num_jobs()

default *args:
    {{forge}} build {{forward}}

help *args:
    {{forge}} --help {{forward}}

list-defconfigs *args:
    {{forge}} list-defconfigs {{forward}}

defconfig *args:
    {{forge}} defconfig {{forward}}

build *args:
    {{forge}} build {{forward}}

rebuild *args:
    {{forge}} rebuild {{forward}}

source *args:
    {{forge}} source {{forward}}

plan *args:
    {{forge}} plan {{forward}}

doctor *args:
    {{forge}} doctor {{forward}}

setup *args:
    {{forge}} setup {{forward}}

run *args:
    {{forge}} run {{forward}}

test-boot *args:
    {{forge}} test-boot {{forward}}

savedefconfig *args:
    {{forge}} savedefconfig {{forward}}

clean *args:
    {{forge}} clean {{forward}}

check:
    cargo fmt --check {{forward}}
    cargo clippy --locked --all-targets -- -D warnings {{forward}}
