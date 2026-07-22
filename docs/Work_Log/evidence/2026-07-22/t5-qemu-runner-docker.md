# T5 Docker static evidence

Container `c238e449081e4c68d07d193d7e7e7c357406503eaab22305cc763a4d9c2e1161` mounted
`/home/pxy/projects/MangoCore-cleanup` at `/app`.

The Docker static gate completed with no QEMU invocation:

```sh
python3 -m py_compile scripts/run_full_test.py scripts/full_test/*.py
sh -n scripts/test-qemu-command-matrix.sh scripts/test-image-role-contract.sh scripts/run_test_docker_parallel.sh
sh scripts/test-qemu-command-matrix.sh
sh scripts/test-image-role-contract.sh
python3 scripts/run_full_test.py --dry-run --serial
```

The command matrix passed RV64/LA64 drive assertions and all seven nonzero failure fixtures;
the image-role contract passed; dry-run printed all seven profiles per architecture.
