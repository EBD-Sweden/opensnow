# One-shot seed image: builds the dbt marts in OpenSnow and exports them to
# Postgres for Metabase. Build context is the repo root.
FROM python:3.12-slim

RUN apt-get update && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*

# dbt + the OpenSnow adapter (from the repo source).
COPY integrations/dbt-opensnow /tmp/dbt-opensnow
RUN pip install --no-cache-dir "dbt-core>=1.8,<2.0" "dbt-postgres>=1.8,<2.0" /tmp/dbt-opensnow

# The demo dbt project (models, macros, sources, profiles).
COPY deploy/demo/dbt /work/dbt
COPY deploy/demo/seed.sh /work/seed.sh
RUN chmod +x /work/seed.sh

WORKDIR /work
ENTRYPOINT ["/work/seed.sh"]
