"""The `af_sampler` sidecar: wraps `codecarbon` (machine energy) and
`psutil` (watched process trees) into Contract #1 `energy_sample` /
`process_sample` events appended to the local spool.

Unlike `af_estimator`, this sidecar does not answer requests on stdout: it
is a *collector*. stdin carries a one-way control stream (`watch`,
`unwatch`, `shutdown`) and everything it observes goes to the spool file.

Run directly as a script (`python __main__.py`), not via `-m`; sidecars are
invoked by script path rather than module name in this PoC.
"""
