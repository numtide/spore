#!/bin/sh
# Makes an AMI from a release disk: `upload.sh NAME DISK.IMG`. coldsnap
# writes the image to an EBS snapshot through the EBS direct API, so no S3
# bucket is needed. Prints the AMI id. Needs coldsnap and the aws CLI, with
# credentials for the account and AWS_DEFAULT_REGION set.
set -eu
name=$1
img=$2
snap=$(coldsnap upload --description "$name" "$img")
coldsnap wait "$snap"
aws ec2 register-image \
  --name "$name" \
  --architecture arm64 \
  --boot-mode uefi \
  --virtualization-type hvm \
  --ena-support \
  --sriov-net-support simple \
  --imds-support v2.0 \
  --root-device-name /dev/xvda \
  --block-device-mappings "DeviceName=/dev/xvda,Ebs={SnapshotId=$snap,VolumeType=gp3,DeleteOnTermination=true}" \
  --query ImageId --output text
