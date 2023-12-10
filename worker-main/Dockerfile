FROM debian:bookworm-slim

RUN apt update; apt install -y libssl3 libcurl4
RUN /sbin/ldconfig

RUN adduser --system jobworkerp

RUN mkdir -p /home/jobworkerp && chown jobworkerp:daemon /home/jobworkerp

WORKDIR /home/jobworkerp

COPY --chown=jobworkerp:daemon ./target/release/ .

USER jobworkerp

CMD ["./worker"]

