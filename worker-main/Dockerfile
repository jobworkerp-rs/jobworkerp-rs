FROM nvcr.io/nvidia/cuda:12.8.1-runtime-ubuntu24.04

RUN apt update && apt install -y libssl3 libcurl4 protobuf-compiler libgomp1 libcudnn9-cuda-12 docker-ce-cli \
    && apt-get clean -y && rm -rf /var/lib/apt/lists/*

RUN adduser --system jobworkerp

RUN mkdir -p /home/jobworkerp && chown jobworkerp:daemon /home/jobworkerp
RUN mkdir -p /home/jobworkerp/plugins && chown jobworkerp:daemon /home/jobworkerp/plugins
ENV LD_LIBRARY_PATH=/home/jobworkerp/plugins:/home/jobworkerp/data/plugin/runner:/home/jobworkerp/data/plugin/cuda_runner:/usr/local/cuda/lib64:$LD_LIBRARY_PATH
RUN /sbin/ldconfig

WORKDIR /home/jobworkerp

COPY --chown=jobworkerp:daemon ./target/release/worker .

USER jobworkerp

CMD ["./worker"]

