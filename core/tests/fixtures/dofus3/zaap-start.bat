set LOGFILE=dofus.%ACCOUNT_ID%.log
set PREVLOGFILE=dofus.prev.%ACCOUNT_ID%.log

if exist "%ZAAP_LOGS_PATH%" (

    if exist "%ZAAP_LOGS_PATH%\%PREVLOGFILE%" (
        del "%ZAAP_LOGS_PATH%\%PREVLOGFILE%"
    )

    if exist "%ZAAP_LOGS_PATH%\%LOGFILE%" (
        move /Y "%ZAAP_LOGS_PATH%\%LOGFILE%" "%ZAAP_LOGS_PATH%\%PREVLOGFILE%"
    )
)


Dofus.exe -logFile "%ZAAP_LOGS_PATH%\%LOGFILE%" %*
