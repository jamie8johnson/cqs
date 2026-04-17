function Get-UserInfo {
    param(
        [string]$Username
    )
    Get-ADUser -Identity $Username
}

function Set-UserStatus {
    param(
        [string]$Username,
        [bool]$Enabled
    )
    Set-ADUser -Identity $Username -Enabled $Enabled
}
