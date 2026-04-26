{ lib, kleyPackage, ... }: {
  environment.systemPackages = [ kleyPackage ];

  kley.agentVm.harnesses = lib.mkAfter [ ];
}
