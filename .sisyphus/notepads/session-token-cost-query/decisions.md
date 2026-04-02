# Decisions

- 2026-04-02: Store only models that expose both `cost.input` and `cost.output`
  in the catalog so the resolver can drop partial pricing as `None` without
  special-case logic elsewhere.
