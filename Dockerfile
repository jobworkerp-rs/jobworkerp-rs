FROM nvcr.io/nvidia/cuda:12.4.1-cudnn-runtime-ubuntu22.04

RUN apt update && apt install -y libssl3 libcurl4 libgomp1 \
    && apt-get clean -y && rm -rf /var/lib/apt/lists/*

RUN adduser --system jobworkerp

RUN mkdir -p /home/jobworkerp && chown jobworkerp:daemon /home/jobworkerp
RUN mkdir -p /home/jobworkerp/plugins && chown jobworkerp:daemon /home/jobworkerp/plugins
ENV LD_LIBRARY_PATH=/home/jobworkerp/plugins:/home/jobworkerp/data/plugin/runner:/home/jobworkerp/data/plugin/cuda_runner:/usr/local/cuda/lib64:$LD_LIBRARY_PATH
RUN /sbin/ldconfig

WORKDIR /home/jobworkerp

COPY --chown=jobworkerp:daemon ./target/release/all-in-one .

USER jobworkerp

CMD ["./all-in-one"]

