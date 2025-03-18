#!/usr/bin/fish

for crate in (cat delete-unused-crates.txt)
    set -l crate_path ./crates/$crate
    if test -d $crate_path
        rm -rf $crate_path
    end
    git add $crate_path
end
