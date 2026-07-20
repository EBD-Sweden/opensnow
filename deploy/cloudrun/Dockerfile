FROM demo-opensnow:latest
USER root
COPY ui /ui
COPY opensnow.toml /etc/opensnow/opensnow.toml
COPY dashboards.json /etc/opensnow/dashboards.json
COPY data /data
COPY entrypoint.sh /entrypoint.sh
RUN chown -R opensnow:opensnow /data
USER opensnow
ENV OPENSNOW_UI_DIR=/ui OPENSNOW_OTEL_DISABLED=1 OPENSNOW_DASHBOARDS_FILE=/etc/opensnow/dashboards.json
ENTRYPOINT ["/entrypoint.sh"]
