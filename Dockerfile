FROM debian:unstable
COPY ./debian_deps.sh .
RUN ./debian_deps.sh
