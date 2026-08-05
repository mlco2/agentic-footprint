// Shared `fetch` binding for AfClient/AllocStore/ReportStore's constructors.
// `fetch` is spec'd with an internal-slot check that throws "Illegal
// invocation" if it's ever called detached from `window` (e.g. stored as a
// plain field and invoked as `this.fetchImpl(...)`, as all three of those
// classes do) — bind it once here rather than repeating the bind (and this
// comment) at every call site.
export const boundFetch: typeof fetch = fetch.bind(globalThis);
