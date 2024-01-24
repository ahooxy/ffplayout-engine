#!/usr/bin/bash

source $(dirname "$0")/man_create.sh
target=$1

if [ ! -f 'ffplayout-frontend/package.json' ]; then
    git submodule update --init
fi

if [[ -n $target ]]; then
    targets=($target)
else
    targets=("x86_64-unknown-linux-musl" "aarch64-unknown-linux-gnu" "x86_64-pc-windows-gnu" "x86_64-apple-darwin" "aarch64-apple-darwin")
fi

IFS="= "
while read -r name value; do
    if [[ $name == "version" ]]; then
        version=${value//\"/}
    fi
done < Cargo.toml

echo "Compile ffplayout \"$version\""
echo ""

for target in "${targets[@]}"; do
    echo "compile static for $target"
    echo ""

    if [[ $target == "x86_64-pc-windows-gnu" ]]; then
        if [[ -f "ffplayout-v${version}_${target}.zip" ]]; then
            rm -f "ffplayout-v${version}_${target}.zip"
        fi

        cargo build --release --target=$target

        cp ./target/${target}/release/ffpapi.exe .
        cp ./target/${target}/release/ffplayout.exe .
        zip -r "ffplayout-v${version}_${target}.zip" assets docker docs LICENSE README.md CHANGELOG.md ffplayout.exe ffpapi.exe -x *.db
        rm -f ffplayout.exe ffpapi.exe
    elif [[ $target == "x86_64-apple-darwin" ]] || [[ $target == "aarch64-apple-darwin" ]]; then
        if [[ -f "ffplayout-v${version}_${target}.tar.gz" ]]; then
            rm -f "ffplayout-v${version}_${target}.tar.gz"
        fi
        c_cc="x86_64-apple-darwin20.4-clang"
        c_cxx="x86_64-apple-darwin20.4-clang++"

        if [[ $target == "aarch64-apple-darwin" ]]; then
            c_cc="aarch64-apple-darwin20.4-clang"
            c_cxx="aarch64-apple-darwin20.4-clang++"
        fi

        CC="$c_cc" CXX="$c_cxx" cargo build --release --target=$target

        cp ./target/${target}/release/ffpapi .
        cp ./target/${target}/release/ffplayout .
        tar -czvf "ffplayout-v${version}_${target}.tar.gz" --exclude='*.db' --exclude='*.db-shm' --exclude='*.db-wal' assets docker docs LICENSE README.md CHANGELOG.md ffplayout ffpapi
        rm -f ffplayout ffpapi
    else
        if [[ -f "ffplayout-v${version}_${target}.tar.gz" ]]; then
            rm -f "ffplayout-v${version}_${target}.tar.gz"
        fi

        cargo build --release --target=$target

        cp ./target/${target}/release/ffpapi .
        cp ./target/${target}/release/ffplayout .
        tar -czvf "ffplayout-v${version}_${target}.tar.gz" --exclude='*.db' --exclude='*.db-shm' --exclude='*.db-wal' assets docker docs LICENSE README.md CHANGELOG.md ffplayout ffpapi
        rm -f ffplayout ffpapi
    fi

    echo ""
done

if [[ "${#targets[@]}" == "3" ]] || [[ $targets == "x86_64-unknown-linux-musl" ]]; then
    cargo deb --target=x86_64-unknown-linux-musl -p ffplayout --manifest-path=ffplayout-engine/Cargo.toml -o ffplayout_${version}-1_amd64.deb
    cargo generate-rpm --payload-compress none  --target=x86_64-unknown-linux-musl -p ffplayout-engine -o ffplayout-${version}-1.x86_64.rpm
fi

if [[ "${#targets[@]}" == "3" ]] || [[ $targets == "aarch64-unknown-linux-gnu" ]]; then
    cargo deb --target=aarch64-unknown-linux-gnu --variant=arm64 -p ffplayout --manifest-path=ffplayout-engine/Cargo.toml -o ffplayout_${version}-1_arm64.deb
fi
