<VirtualHost 127.0.0.1:{{PORT}} [::1]:{{PORT}}>
    ServerName {{SERVER_NAME}}
    ServerAlias {{SERVER_ALIAS}}
    DocumentRoot "{{ROOT}}"
    <Directory "{{ROOT}}">
        Options {{OPTIONS}} FollowSymLinks
        AllowOverride All
        Require all granted
    </Directory>
{{TOOLS}}
{{SSL}}
</VirtualHost>
