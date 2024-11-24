FROM nvcr.io/nvidia/cuda:12.4.1-cudnn-runtime-ubuntu22.04

RUN apt update && apt install -y libssl3 libcurl4  \
    && apt-get clean -y && rm -rf /var/lib/apt/lists/*

RUN /sbin/ldconfig

RUN adduser --system jobworkerp

RUN mkdir -p /home/jobworkerp && chown jobworkerp:daemon /home/jobworkerp

WORKDIR /home/jobworkerp

COPY --chown=jobworkerp:daemon ./target/release/ .

USER jobworkerp

CMD ["./all-in-one"]

