{
  lib,
  buildPythonApplication,
  fetchPypi,
  setuptools,
  sqlitedict,
  websockets,
}:

buildPythonApplication rec {
  pname = "bscpylgtv";
  version = "0.5.2";
  pyproject = true;

  src = fetchPypi {
    inherit pname version;
    hash = "sha256-IU6wPH7XFtKEmR8+JeGA9qBcU4YmzQdRlYiZhX1Gj2c=";
  };

  build-system = [ setuptools ];
  dependencies = [
    sqlitedict
    websockets
  ];

  doCheck = false;
  pythonImportsCheck = [ "bscpylgtv" ];

  meta = {
    description = "Python command helper for controlling LG webOS TVs";
    homepage = "https://github.com/chros73/bscpylgtv";
    license = lib.licenses.mit;
    mainProgram = "bscpylgtvcommand";
    platforms = lib.platforms.linux;
  };
}
