{ sourceResolution }:
{
  canaryHost = "saga-dev2";
  baselineHost = "saga-dev";
  updateDriver = "flake.lock";
  sharedModuleGraph = true;
  defaultCheckoutRef = "HEAD";

  source = {
    exactRevision = sourceResolution.kley.exactRevision;
    shortRevision = sourceResolution.kley.shortRevision;
    lastModified = sourceResolution.kley.lastModified;
  };

  resolvedInputs = {
    nixpkgs = sourceResolution.nixpkgs;
  };
}
