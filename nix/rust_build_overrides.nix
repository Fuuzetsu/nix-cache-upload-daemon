pkgs:

let
  buildInputOverrides = { };

  # Overrides of things other than build inputs.
  otherOverrides = { };
  buildAttr = getAttr: pkgs.lib.concatMap getAttr (builtins.attrValues buildInputOverrides);
in
rec
{
  nativeBuildInputs = buildAttr (c: c.nativeBuildInputs or [ ]);
  buildInputs = buildAttr (c: c.buildInputs or [ ]);
  # Join build and other overrides. To do this, start with set of all other
  # overrides then merge into it the build overrides. We only need to care about
  # case where an attribute is present in both. In this case, we'll be replacing
  # otherOverride with a buildOverride but with a catch: we'll also join the
  # otherOverride attributes into it. This should basically result in an
  # unionWith sort of behaviour.
  defaultCrateOverrides = otherOverrides // builtins.mapAttrs
    (n: buildOverride: attrs:
      if otherOverrides ? ${n}
      then otherOverrides.${n} attrs // buildOverride
      else buildOverride
    )
    buildInputOverrides;
}
